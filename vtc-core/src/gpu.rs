//! GPU packing and unpacking with Direct3D 11 shaders (see shaders.hlsl).
//!
//!   Packer   (sender):   Spout texture → [GPU] → packed NV12 → read back for the encoder
//!   Unpacker (receiver): decoded NV12 → upload → [GPU] → straight into a Spout texture
//!
//! Compared with doing this on the CPU, the CPU now only copies a compact 6 MB
//! NV12 frame once each way, instead of converting ~16 MB RGBA frames pixel by pixel.

use std::ffi::c_void;

use windows::core::{PCSTR, Result, s};
use windows::Win32::Foundation::{E_FAIL, E_NOTIMPL};
use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::{D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST, ID3DBlob};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;

use crate::grab::open_shared;
use crate::spout::SharedTextureInfo;

/// The shader source is built into our program at compile time.
const SHADER_SOURCE: &str = include_str!("shaders.hlsl");

/// Byte layout of a packed NV12 frame in memory (matches GStreamer's layout).
#[derive(Clone, Copy, Debug)]
pub struct Nv12Layout {
    pub y_offset: usize,
    pub y_stride: usize,
    pub uv_offset: usize,
    pub uv_stride: usize,
}

// ---------------- shared helpers ----------------

/// Compile one function from shaders.hlsl. `target` = shader type and version,
/// e.g. "ps_5_0" (pixel shader, Direct3D 11 level).
pub(crate) fn compile(entry: PCSTR, target: PCSTR) -> Result<ID3DBlob> {
    let mut code: Option<ID3DBlob> = None;
    let mut errors: Option<ID3DBlob> = None;
    let result = unsafe {
        D3DCompile(
            SHADER_SOURCE.as_ptr() as *const c_void,
            SHADER_SOURCE.len(),
            s!("shaders.hlsl"),
            None,
            None,
            entry,
            target,
            0,
            0,
            &mut code,
            Some(&mut errors),
        )
    };
    if let Err(e) = result {
        // Show the compiler's message (which line is wrong and why).
        let msg = errors.map(|b| blob_text(&b)).unwrap_or_default();
        return Err(windows::core::Error::new(e.code(), format!("shader compile failed: {msg}")));
    }
    Ok(code.unwrap())
}

pub(crate) fn blob_bytes(blob: &ID3DBlob) -> &[u8] {
    unsafe { std::slice::from_raw_parts(blob.GetBufferPointer() as *const u8, blob.GetBufferSize()) }
}

fn blob_text(blob: &ID3DBlob) -> String {
    String::from_utf8_lossy(blob_bytes(blob)).trim_end_matches('\0').to_string()
}

fn vertex_shader(device: &ID3D11Device) -> Result<ID3D11VertexShader> {
    let code = compile(s!("VSFullscreen"), s!("vs_5_0"))?;
    let mut vs = None;
    unsafe { device.CreateVertexShader(blob_bytes(&code), None, Some(&mut vs))? };
    Ok(vs.unwrap())
}

pub(crate) fn pixel_shader(device: &ID3D11Device, entry: PCSTR) -> Result<ID3D11PixelShader> {
    let code = compile(entry, s!("ps_5_0"))?;
    let mut ps = None;
    unsafe { device.CreatePixelShader(blob_bytes(&code), None, Some(&mut ps))? };
    Ok(ps.unwrap())
}

/// The 32-byte `Params` block the shaders read (see shaders.hlsl): transmit size,
/// source size, and where the source fits in the transmit frame.
fn params_buffer(device: &ID3D11Device, transmit: (u32, u32), source: (u32, u32), fit: [f32; 4]) -> Result<ID3D11Buffer> {
    let mut data = [0u32; 8];
    data[..4].copy_from_slice(&[transmit.0, transmit.1, source.0, source.1]);
    for (i, v) in fit.iter().enumerate() {
        data[4 + i] = v.to_bits(); // floats, stored bit-for-bit
    }
    let desc = D3D11_BUFFER_DESC {
        ByteWidth: 32,
        Usage: D3D11_USAGE_IMMUTABLE,
        BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
        ..Default::default()
    };
    let init = D3D11_SUBRESOURCE_DATA { pSysMem: data.as_ptr() as *const c_void, ..Default::default() };
    let mut buf = None;
    unsafe { device.CreateBuffer(&desc, Some(&init), Some(&mut buf))? };
    Ok(buf.unwrap())
}

/// A plain 2D texture. `bind` = how the GPU may use it; `staging` = CPU-readable copy.
pub(crate) fn texture(device: &ID3D11Device, w: u32, h: u32, format: DXGI_FORMAT, bind: u32, staging: bool) -> Result<ID3D11Texture2D> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: format,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: if staging { D3D11_USAGE_STAGING } else { D3D11_USAGE_DEFAULT },
        BindFlags: bind,
        CPUAccessFlags: if staging { D3D11_CPU_ACCESS_READ.0 as u32 } else { 0 },
        MiscFlags: 0,
    };
    let mut tex = None;
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut tex))? };
    Ok(tex.unwrap())
}

pub(crate) fn srv(device: &ID3D11Device, tex: &ID3D11Texture2D) -> Result<ID3D11ShaderResourceView> {
    let mut view = None;
    unsafe { device.CreateShaderResourceView(tex, None, Some(&mut view))? };
    Ok(view.unwrap())
}

pub(crate) fn rtv(device: &ID3D11Device, tex: &ID3D11Texture2D) -> Result<ID3D11RenderTargetView> {
    let mut view = None;
    unsafe { device.CreateRenderTargetView(tex, None, Some(&mut view))? };
    Ok(view.unwrap())
}

/// Run one pixel shader over a whole `w`×`h` render target.
#[allow(clippy::too_many_arguments)]
fn draw(
    ctx: &ID3D11DeviceContext,
    vs: &ID3D11VertexShader,
    ps: &ID3D11PixelShader,
    params: &ID3D11Buffer,
    inputs: &[Option<ID3D11ShaderResourceView>],
    target: &ID3D11RenderTargetView,
    w: u32,
    h: u32,
) {
    unsafe {
        ctx.IASetInputLayout(None);
        ctx.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        ctx.VSSetShader(vs, None);
        ctx.PSSetShader(ps, None);
        ctx.PSSetConstantBuffers(0, Some(&[Some(params.clone())]));
        ctx.PSSetShaderResources(0, Some(inputs));
        ctx.OMSetRenderTargets(Some(&[Some(target.clone())]), None);
        ctx.RSSetViewports(Some(&[D3D11_VIEWPORT {
            TopLeftX: 0.0,
            TopLeftY: 0.0,
            Width: w as f32,
            Height: h as f32,
            MinDepth: 0.0,
            MaxDepth: 1.0,
        }]));
        ctx.Draw(3, 0);
        // Unbind, so these textures can be written/read elsewhere next.
        ctx.OMSetRenderTargets(None, None);
        let none: Vec<Option<ID3D11ShaderResourceView>> = vec![None; inputs.len()];
        ctx.PSSetShaderResources(0, Some(&none));
    }
}

// ---------------- Packer (sender side) ----------------

/// Reads a Spout sender's texture and turns it into a packed NV12 frame, on the GPU.
pub struct Packer {
    _device: ID3D11Device,
    context: ID3D11DeviceContext,
    avatar_srv: ID3D11ShaderResourceView,
    vs: ID3D11VertexShader,
    ps_luma: ID3D11PixelShader,
    ps_chroma: ID3D11PixelShader,
    params: ID3D11Buffer,
    luma: ID3D11Texture2D,
    chroma: ID3D11Texture2D,
    luma_rtv: ID3D11RenderTargetView,
    chroma_rtv: ID3D11RenderTargetView,
    luma_staging: ID3D11Texture2D,
    chroma_staging: ID3D11Texture2D,
    /// Transmit size (what's sent; the packed frame is twice as wide).
    pub width: u32,
    pub height: u32,
    /// The VTubing app's own image size.
    pub source_width: u32,
    pub source_height: u32,
    /// The Spout handle we opened (reopen if the sender publishes a new one).
    pub share_handle: u32,
}

impl Packer {
    /// Read the Spout sender `info` and pack it at `transmit` size (width, height; both
    /// rounded down to even numbers). The source is scaled to fit, keeping its shape,
    /// centred and resting on the bottom edge; any leftover space is see-through.
    pub fn open(info: &SharedTextureInfo, transmit: (u32, u32)) -> Result<Packer> {
        // Opens it on whichever graphics card the VTubing app uses (laptops may have two).
        let (device, context, avatar) = open_shared(info.share_handle)?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { avatar.GetDesc(&mut desc) };
        let (sw, sh) = (desc.Width.max(1), desc.Height.max(1));
        // H.264 (4:2:0) needs even sizes.
        let (w, h) = (transmit.0 & !1, transmit.1 & !1);
        if w == 0 || h == 0 {
            return Err(windows::core::Error::new(E_NOTIMPL, "transmit size too small"));
        }
        let scale = (w as f32 / sw as f32).min(h as f32 / sh as f32);
        let (fw, fh) = (sw as f32 * scale, sh as f32 * scale);
        let fit = [(w as f32 - fw) / 2.0, h as f32 - fh, fw, fh];

        let rt = (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32;
        let luma = texture(&device, w * 2, h, DXGI_FORMAT_R8_UNORM, rt, false)?;
        let chroma = texture(&device, w, h / 2, DXGI_FORMAT_R8G8_UNORM, rt, false)?;

        Ok(Packer {
            avatar_srv: srv(&device, &avatar)?,
            vs: vertex_shader(&device)?,
            ps_luma: pixel_shader(&device, s!("PSPackLuma"))?,
            ps_chroma: pixel_shader(&device, s!("PSPackChroma"))?,
            params: params_buffer(&device, (w, h), (sw, sh), fit)?,
            luma_rtv: rtv(&device, &luma)?,
            chroma_rtv: rtv(&device, &chroma)?,
            luma_staging: texture(&device, w * 2, h, DXGI_FORMAT_R8_UNORM, 0, true)?,
            chroma_staging: texture(&device, w, h / 2, DXGI_FORMAT_R8G8_UNORM, 0, true)?,
            luma,
            chroma,
            _device: device,
            context,
            width: w,
            height: h,
            source_width: sw,
            source_height: sh,
            share_handle: info.share_handle,
        })
    }

    /// Pack the avatar's current frame and copy it into `out` (an NV12 buffer).
    pub fn pack_nv12(&mut self, out: &mut [u8], layout: Nv12Layout) -> Result<()> {
        let (w, h) = (self.width, self.height);
        let ctx = &self.context;
        let input = [Some(self.avatar_srv.clone())];
        draw(ctx, &self.vs, &self.ps_luma, &self.params, &input, &self.luma_rtv, w * 2, h);
        draw(ctx, &self.vs, &self.ps_chroma, &self.params, &input, &self.chroma_rtv, w, h / 2);
        unsafe {
            ctx.CopyResource(&self.luma_staging, &self.luma);
            ctx.CopyResource(&self.chroma_staging, &self.chroma);
        }
        // Y plane: (2w) bytes per row, h rows. UV plane: (2w) bytes per row, h/2 rows.
        read_plane(ctx, &self.luma_staging, out, layout.y_offset, layout.y_stride, (w * 2) as usize, h as usize)?;
        read_plane(ctx, &self.chroma_staging, out, layout.uv_offset, layout.uv_stride, (w * 2) as usize, (h / 2) as usize)?;
        Ok(())
    }
}

/// Copy a CPU-readable texture's rows into `out` at `offset`, `stride` bytes apart.
fn read_plane(
    ctx: &ID3D11DeviceContext,
    staging: &ID3D11Texture2D,
    out: &mut [u8],
    offset: usize,
    stride: usize,
    row_bytes: usize,
    rows: usize,
) -> Result<()> {
    if out.len() < offset + stride * (rows - 1) + row_bytes {
        return Err(windows::core::Error::new(E_FAIL, "NV12 output buffer too small"));
    }
    unsafe {
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // Waits here until the GPU has finished drawing and copying.
        ctx.Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
        let src = mapped.pData as *const u8;
        for y in 0..rows {
            let row = std::slice::from_raw_parts(src.add(y * mapped.RowPitch as usize), row_bytes);
            out[offset + y * stride..offset + y * stride + row_bytes].copy_from_slice(row);
        }
        ctx.Unmap(staging, 0);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every shader function in shaders.hlsl must compile.
    #[test]
    fn shaders_compile() {
        for (entry, target) in [
            (s!("VSFullscreen"), s!("vs_5_0")),
            (s!("PSPackLuma"), s!("ps_5_0")),
            (s!("PSPackChroma"), s!("ps_5_0")),
            (s!("PSUnpack"), s!("ps_5_0")),
            (s!("VSFullscreenUV"), s!("vs_5_0")),
            (s!("PSLayer"), s!("ps_5_0")),
            (s!("PSResolve"), s!("ps_5_0")),
        ] {
            if let Err(e) = compile(entry, target) {
                panic!("{}", e.message());
            }
        }
    }
}

// ---------------- Unpacker (receiver side) ----------------

/// Uploads a decoded packed NV12 frame and draws the transparent avatar into a
/// render target (normally a Spout sender's texture), on the GPU. The avatar is
/// stretched to the target's size if a partner sent it smaller.
pub struct Unpacker {
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    params: ID3D11Buffer,
    luma: ID3D11Texture2D,
    chroma: ID3D11Texture2D,
    inputs: [Option<ID3D11ShaderResourceView>; 2],
    /// Avatar size as received (the packed frame is twice as wide).
    pub width: u32,
    pub height: u32,
    /// Size it's drawn at (the render target's size).
    pub out: (u32, u32),
}

impl Unpacker {
    /// Must use the same `device` as the render target it will draw into.
    /// `width`×`height` = received avatar size; `out` = render target size.
    pub fn new(device: &ID3D11Device, width: u32, height: u32, out: (u32, u32)) -> Result<Unpacker> {
        let sr = D3D11_BIND_SHADER_RESOURCE.0 as u32;
        let luma = texture(device, width * 2, height, DXGI_FORMAT_R8_UNORM, sr, false)?;
        let chroma = texture(device, width, height / 2, DXGI_FORMAT_R8G8_UNORM, sr, false)?;
        Ok(Unpacker {
            vs: vertex_shader(device)?,
            ps: pixel_shader(device, s!("PSUnpack"))?,
            // For unpacking, the "source size" slots carry the output size.
            params: params_buffer(device, (width, height), out, [0.0; 4])?,
            inputs: [Some(srv(device, &luma)?), Some(srv(device, &chroma)?)],
            luma,
            chroma,
            width,
            height,
            out,
        })
    }

    /// Upload the two NV12 planes and draw the avatar into `target`.
    pub fn unpack(
        &self,
        ctx: &ID3D11DeviceContext,
        target: &ID3D11RenderTargetView,
        y: &[u8],
        y_stride: usize,
        uv: &[u8],
        uv_stride: usize,
    ) -> Result<()> {
        let (w, h) = (self.width as usize, self.height as usize);
        if y.len() < y_stride * (h - 1) + w * 2 || uv.len() < uv_stride * (h / 2 - 1) + w * 2 {
            return Err(windows::core::Error::new(E_FAIL, "NV12 frame smaller than expected"));
        }
        unsafe {
            ctx.UpdateSubresource(&self.luma, 0, None, y.as_ptr() as *const c_void, y_stride as u32, 0);
            ctx.UpdateSubresource(&self.chroma, 0, None, uv.as_ptr() as *const c_void, uv_stride as u32, 0);
        }
        draw(ctx, &self.vs, &self.ps, &self.params, &self.inputs, target, self.out.0, self.out.1);
        Ok(())
    }
}
