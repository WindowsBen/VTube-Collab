//! The group source: several Spout sources stacked into one, on the GPU.
//!
//! Each frame:
//!   1. clear an "accumulation" texture to fully transparent
//!   2. draw each layer, bottom first, scaled to fit the canvas (keeping its shape),
//!      blended on top of what's there (PSLayer in shaders.hlsl)
//!   3. convert the result to normal alpha straight into our Spout output (PSResolve)
//!
//! Layers are other Spout sources (your avatar, your partners' "Collab - ..." sources),
//! opened by name — so the compositor doesn't need to know where they come from.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use windows::core::{Result, s};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Graphics::Direct3D::D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;

use crate::gpu::{blob_bytes, compile, pixel_shader, rtv, srv, texture};
use crate::sender::{NamedMutex, SpoutSender};
use crate::spout;

/// How often to re-check each layer's Spout details (it may restart or resize).
const RECHECK_EVERY: Duration = Duration::from_secs(1);

/// One opened layer source.
struct Source {
    share_handle: u32,
    view: ID3D11ShaderResourceView,
    width: u32,
    height: u32,
    /// Spout's per-sender lock, so we don't read while its owner is writing.
    access: NamedMutex,
    checked: Instant,
}

pub struct Compositor {
    output: SpoutSender,
    vs: ID3D11VertexShader,
    ps_layer: ID3D11PixelShader,
    ps_resolve: ID3D11PixelShader,
    sampler: ID3D11SamplerState,
    blend: ID3D11BlendState,
    accum_rtv: ID3D11RenderTargetView,
    accum_srv: ID3D11ShaderResourceView,
    sources: HashMap<String, Source>,
    pub width: u32,
    pub height: u32,
}

impl Compositor {
    /// Publish a `width`×`height` Spout source called `name` (or "name 2" if taken).
    pub fn new(name: &str, width: u32, height: u32) -> Result<Compositor> {
        let output = SpoutSender::new(&crate::sender::unused_name(name), width, height)?;
        let device = output.device().clone();

        let vs = {
            let code = compile(s!("VSFullscreenUV"), s!("vs_5_0"))?;
            let mut vs = None;
            unsafe { device.CreateVertexShader(blob_bytes(&code), None, Some(&mut vs))? };
            vs.unwrap()
        };

        // Smooth (bilinear) scaling; edges clamp instead of wrapping around.
        let sampler = unsafe {
            let desc = D3D11_SAMPLER_DESC {
                Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
                AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
                MaxLOD: f32::MAX,
                ComparisonFunc: D3D11_COMPARISON_NEVER,
                ..Default::default()
            };
            let mut sampler = None;
            device.CreateSamplerState(&desc, Some(&mut sampler))?;
            sampler.unwrap()
        };

        // "Over" blending for premultiplied colour: result = layer + existing × (1 − layer alpha).
        let blend = unsafe {
            let mut desc = D3D11_BLEND_DESC::default();
            desc.RenderTarget[0] = D3D11_RENDER_TARGET_BLEND_DESC {
                BlendEnable: true.into(),
                SrcBlend: D3D11_BLEND_ONE,
                DestBlend: D3D11_BLEND_INV_SRC_ALPHA,
                BlendOp: D3D11_BLEND_OP_ADD,
                SrcBlendAlpha: D3D11_BLEND_ONE,
                DestBlendAlpha: D3D11_BLEND_INV_SRC_ALPHA,
                BlendOpAlpha: D3D11_BLEND_OP_ADD,
                RenderTargetWriteMask: D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8,
            };
            let mut blend = None;
            device.CreateBlendState(&desc, Some(&mut blend))?;
            blend.unwrap()
        };

        // Accumulation texture: 16-bit floats per channel, so stacking stays precise.
        let rt = (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32;
        let accum = texture(&device, width, height, DXGI_FORMAT_R16G16B16A16_FLOAT, rt, false)?;

        Ok(Compositor {
            vs,
            ps_layer: pixel_shader(&device, s!("PSLayer"))?,
            ps_resolve: pixel_shader(&device, s!("PSResolve"))?,
            sampler,
            blend,
            accum_rtv: rtv(&device, &accum)?,
            accum_srv: srv(&device, &accum)?,
            sources: HashMap::new(),
            output,
            width,
            height,
        })
    }

    /// The name the group source is published under.
    pub fn name(&self) -> &str {
        self.output.name()
    }

    /// Draw one frame. `layers` = Spout source names, **bottom layer first**.
    /// Sources that aren't running (yet) are simply skipped.
    pub fn render(&mut self, layers: &[String]) -> Result<()> {
        for name in layers {
            self.refresh_source(name);
        }
        // Forget sources no longer used as layers.
        self.sources.retain(|name, _| layers.contains(name));

        let (w, h) = (self.width, self.height);
        let Compositor { output, vs, ps_layer, ps_resolve, sampler, blend, accum_rtv, accum_srv, sources, .. } = self;

        output.render(|ctx, target| {
            unsafe {
                ctx.ClearRenderTargetView(&*accum_rtv, &[0.0, 0.0, 0.0, 0.0]);
                ctx.IASetInputLayout(None);
                ctx.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
                ctx.VSSetShader(&*vs, None);

                // 1–2. Each layer, bottom first, blended into the accumulation texture.
                ctx.PSSetShader(&*ps_layer, None);
                ctx.PSSetSamplers(0, Some(&[Some(sampler.clone())]));
                ctx.OMSetBlendState(&*blend, None, 0xFFFF_FFFF);
                ctx.OMSetRenderTargets(Some(&[Some(accum_rtv.clone())]), None);
                for name in layers.iter() {
                    let Some(src) = sources.get(name) else { continue };
                    // Hold the source's lock while the GPU reads it (skip waiting if busy).
                    let _guard = src.access.lock(2);
                    ctx.RSSetViewports(Some(&[fit(src.width, src.height, w, h)]));
                    ctx.PSSetShaderResources(0, Some(&[Some(src.view.clone())]));
                    ctx.Draw(3, 0);
                    ctx.PSSetShaderResources(0, Some(&[None]));
                }

                // 3. Convert to straight alpha, straight into our Spout texture.
                ctx.OMSetBlendState(None, None, 0xFFFF_FFFF);
                ctx.OMSetRenderTargets(Some(&[Some(target.clone())]), None);
                ctx.RSSetViewports(Some(&[fit(w, h, w, h)]));
                ctx.PSSetShader(&*ps_resolve, None);
                ctx.PSSetShaderResources(0, Some(&[Some(accum_srv.clone())]));
                ctx.Draw(3, 0);
                ctx.PSSetShaderResources(0, Some(&[None]));
                ctx.OMSetRenderTargets(None, None);
            }
            Ok(())
        })
    }

    /// Open a layer's Spout texture, or reopen it if the sender restarted.
    fn refresh_source(&mut self, name: &str) {
        if let Some(src) = self.sources.get(name) {
            if src.checked.elapsed() < RECHECK_EVERY {
                return;
            }
        }
        let Some(info) = spout::read_sender_info(name) else {
            self.sources.remove(name); // not running (anymore)
            return;
        };
        if let Some(src) = self.sources.get_mut(name) {
            src.checked = Instant::now();
            if src.share_handle == info.share_handle {
                return; // unchanged
            }
        }
        match self.open_source(name, info.share_handle) {
            Ok(src) => {
                self.sources.insert(name.to_string(), src);
            }
            Err(_) => {
                self.sources.remove(name);
            }
        }
    }

    fn open_source(&self, name: &str, share_handle: u32) -> Result<Source> {
        let device = self.output.device();
        let tex: ID3D11Texture2D = unsafe {
            let mut tex = None;
            device.OpenSharedResource(HANDLE(share_handle as usize as *mut _), &mut tex)?;
            tex.unwrap()
        };
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { tex.GetDesc(&mut desc) };
        Ok(Source {
            share_handle,
            view: srv(device, &tex)?,
            width: desc.Width,
            height: desc.Height,
            access: NamedMutex::new(&format!("{name}_SpoutAccessMutex"))?,
            checked: Instant::now(),
        })
    }
}

/// The largest area of a `cw`×`ch` canvas that fits a `sw`×`sh` image without
/// distorting it, centred horizontally and resting on the bottom edge (avatars stand
/// on the "floor").
fn fit(sw: u32, sh: u32, cw: u32, ch: u32) -> D3D11_VIEWPORT {
    let scale = (cw as f32 / sw.max(1) as f32).min(ch as f32 / sh.max(1) as f32);
    let (w, h) = (sw as f32 * scale, sh as f32 * scale);
    D3D11_VIEWPORT {
        TopLeftX: (cw as f32 - w) / 2.0,
        TopLeftY: ch as f32 - h,
        Width: w,
        Height: h,
        MinDepth: 0.0,
        MaxDepth: 1.0,
    }
}
