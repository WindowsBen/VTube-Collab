// GPU programs ("shaders") for packing and unpacking transparent frames.
// Language: HLSL (Direct3D's shader language). Compiled at runtime by Windows.
//
// How a shader runs: we ask the GPU to draw one triangle that covers the whole
// output image. For every output pixel, the GPU runs a "pixel shader" function
// (PS...) in parallel, which returns that pixel's value.
//
// Video format: NV12 = the format H.264 encoders and decoders use natively.
//   * plane 1 "Y" (luma/brightness): one byte per pixel, full size
//   * plane 2 "UV" (chroma/colour):  two bytes per 2×2 block of pixels
// Values use BT.709 "limited range": brightness 16..235, colour 16..240 (centre 128).
//
// Packed frame layout (twice as wide as the avatar):
//   left half  = avatar colours (fully transparent pixels made black)
//   right half = avatar alpha as brightness (black = see-through, white = solid)

cbuffer Params : register(b0)
{
    uint avatarWidth; // width of the avatar image (half the packed width)
    uint3 padding;    // constant buffers come in 16-byte blocks
};

// ---- Vertex shader: makes one triangle that covers the whole output ----
// No vertex data needed: positions are computed from the vertex number (0, 1, 2).
float4 VSFullscreen(uint id : SV_VertexID) : SV_Position
{
    float2 uv = float2((id << 1) & 2, id & 2);
    return float4(uv * float2(2, -2) + float2(-1, 1), 0, 1);
}

static const float3 TO_Y  = float3( 0.2126,  0.7152,  0.0722);
static const float3 TO_CB = float3(-0.1146, -0.3854,  0.5   );
static const float3 TO_CR = float3( 0.5,    -0.4542, -0.0458);

// ================= PACK (sender side) =================

Texture2D<float4> avatar : register(t0); // the avatar's Spout texture (RGBA)

float3 colourAt(int2 p)
{
    float4 c = avatar.Load(int3(p, 0));
    return c.a > 0.0 ? c.rgb : float3(0, 0, 0); // see-through → black
}

// Writes the Y plane of the packed frame (packed width × height).
float PSPackLuma(float4 pos : SV_Position) : SV_Target
{
    int2 p = int2(pos.xy);
    if (p.x < (int)avatarWidth)
        return (16.0 + 219.0 * dot(colourAt(p), TO_Y)) / 255.0;          // colour half
    float a = avatar.Load(int3(p.x - (int)avatarWidth, p.y, 0)).a;
    return (16.0 + 219.0 * a) / 255.0;                                    // alpha half
}

// Writes the UV plane (half packed width × half height): one value per 2×2 block.
float2 PSPackChroma(float4 pos : SV_Position) : SV_Target
{
    int2 q = int2(pos.xy) * 2; // top-left pixel of this 2×2 block
    if (q.x >= (int)avatarWidth)
        return float2(128.0, 128.0) / 255.0;                              // alpha half: grey
    float3 rgb = (colourAt(q) + colourAt(q + int2(1, 0)) +
                  colourAt(q + int2(0, 1)) + colourAt(q + int2(1, 1))) * 0.25;
    return float2(128.0 + 224.0 * dot(rgb, TO_CB),
                  128.0 + 224.0 * dot(rgb, TO_CR)) / 255.0;
}

// ================= UNPACK (receiver side) =================

Texture2D<float>  lumaPlane   : register(t0); // decoded Y plane
Texture2D<float2> chromaPlane : register(t1); // decoded UV plane

// Writes the avatar (avatar width × height), RGBA with transparency.
float4 PSUnpack(float4 pos : SV_Position) : SV_Target
{
    int2 p = int2(pos.xy);
    float  y = (lumaPlane.Load(int3(p, 0)) * 255.0 - 16.0) / 219.0;
    float2 c = (chromaPlane.Load(int3(p / 2, 0)) * 255.0 - 128.0) / 224.0; // (Cb, Cr)
    float3 rgb = float3(y + 1.5748 * c.y,
                        y - 0.1873 * c.x - 0.4681 * c.y,
                        y + 1.8556 * c.x);

    float a = saturate((lumaPlane.Load(int3(p.x + (int)avatarWidth, p.y, 0)) * 255.0 - 16.0) / 219.0);
    // Video compression adds tiny noise. Snap values within 2 steps of the ends so
    // see-through stays exactly 0 and solid stays exactly 255 (no faint haze).
    if (a <= 2.0 / 255.0) a = 0.0;
    if (a >= 253.0 / 255.0) a = 1.0;
    return float4(saturate(rgb), a);
}

// ================= GROUP SOURCE (compositor) =================
//
// Layers are blended one by one into an "accumulation" texture, bottom layer first,
// in *premultiplied* form (colour already multiplied by alpha). That makes stacking
// see-through avatars exact. A final pass converts back to normal ("straight")
// alpha, which is what OBS expects from Spout sources.

struct VOut
{
    float4 pos : SV_Position;
    float2 uv  : TEXCOORD0; // 0..1 across the area being drawn
};

// Like VSFullscreen, but also tells the pixel shader where it is (0..1) in the
// area being drawn, so a layer can be stretched to fit that area.
VOut VSFullscreenUV(uint id : SV_VertexID)
{
    float2 uv = float2((id << 1) & 2, id & 2);
    VOut o;
    o.pos = float4(uv * float2(2, -2) + float2(-1, 1), 0, 1);
    o.uv = uv;
    return o;
}

Texture2D<float4> layerTexture : register(t0);
SamplerState      smoothSampler : register(s0); // blends neighbouring pixels when scaling

// One layer, premultiplied. Blending (One, 1 - source alpha) stacks it on top.
float4 PSLayer(VOut v) : SV_Target
{
    float4 c = layerTexture.Sample(smoothSampler, v.uv);
    return float4(c.rgb * c.a, c.a);
}

Texture2D<float4> accumulated : register(t0);

// Final pass: premultiplied → straight alpha for the Spout output.
float4 PSResolve(float4 pos : SV_Position) : SV_Target
{
    float4 p = accumulated.Load(int3(pos.xy, 0));
    return p.a > 0.0 ? float4(saturate(p.rgb / p.a), saturate(p.a)) : float4(0, 0, 0, 0);
}
