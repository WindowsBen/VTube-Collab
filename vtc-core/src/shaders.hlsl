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
//
// The avatar is sent at a fixed "transmit size" (e.g. 1280×720 in bigger rooms).
// When packing, the source image is scaled to fit inside it — so any source size
// works, and the source may even change size mid-collab without anything restarting.

cbuffer Params : register(b0)
{
    uint avatarWidth;  // transmit width (half the packed width)
    uint avatarHeight; // transmit height
    uint srcWidth;     // pack: the VTubing app's image size; unpack: the output size
    uint srcHeight;
    float4 fit;        // where the source lands in the transmit frame: x, y, width, height (pixels)
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

// The avatar at transmit pixel `p` (colour + alpha), scaled from the source image.
// Blends the 4 nearest source pixels ("bilinear"), weighting colours by their alpha
// ("premultiplied") so see-through pixels don't darken the avatar's edges.
float4 avatarAt(int2 p)
{
    float2 local = (float2(p) + 0.5 - fit.xy) / fit.zw;   // 0..1 inside the fitted area
    if (any(local < 0.0) || any(local > 1.0))
        return float4(0, 0, 0, 0);                          // outside: see-through
    float2 s = local * float2(srcWidth, srcHeight) - 0.5;  // position in source pixels
    int2 i0 = int2(floor(s));
    float2 f = s - floor(s);
    int2 last = int2(srcWidth, srcHeight) - 1;
    float4 acc = float4(0, 0, 0, 0);
    [unroll] for (int dy = 0; dy < 2; dy++)
    {
        [unroll] for (int dx = 0; dx < 2; dx++)
        {
            float4 c = avatar.Load(int3(clamp(i0 + int2(dx, dy), int2(0, 0), last), 0));
            float w = (dx ? f.x : 1.0 - f.x) * (dy ? f.y : 1.0 - f.y);
            acc += w * float4(c.rgb * c.a, c.a);
        }
    }
    return acc.a > 0.0 ? float4(acc.rgb / acc.a, acc.a) : float4(0, 0, 0, 0);
}

float3 colourAt(int2 p)
{
    return avatarAt(p).rgb; // see-through pixels come back black already
}

// Writes the Y plane of the packed frame (packed width × height).
float PSPackLuma(float4 pos : SV_Position) : SV_Target
{
    int2 p = int2(pos.xy);
    if (p.x < (int)avatarWidth)
        return (16.0 + 219.0 * dot(colourAt(p), TO_Y)) / 255.0;          // colour half
    float a = avatarAt(int2(p.x - (int)avatarWidth, p.y)).a;
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

// The received avatar at pixel `p`, premultiplied (colour × alpha), so blending
// neighbours when stretching doesn't darken the edges.
float4 unpackAt(int2 p)
{
    p = clamp(p, int2(0, 0), int2(avatarWidth, avatarHeight) - 1);
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
    return float4(saturate(rgb) * a, a);
}

// Writes the avatar at the output size (srcWidth × srcHeight here), RGBA with
// transparency. If the partner sent it smaller, it's stretched back up, blending
// the 4 nearest received pixels. At equal sizes this is an exact 1:1 copy.
float4 PSUnpack(float4 pos : SV_Position) : SV_Target
{
    float2 s = pos.xy * float2(avatarWidth, avatarHeight) / float2(srcWidth, srcHeight) - 0.5;
    int2 i0 = int2(floor(s));
    float2 f = s - floor(s);
    float4 acc = (1.0 - f.x) * (1.0 - f.y) * unpackAt(i0)
               + f.x * (1.0 - f.y)         * unpackAt(i0 + int2(1, 0))
               + (1.0 - f.x) * f.y         * unpackAt(i0 + int2(0, 1))
               + f.x * f.y                 * unpackAt(i0 + int2(1, 1));
    return acc.a > 0.0 ? float4(saturate(acc.rgb / acc.a), acc.a) : float4(0, 0, 0, 0);
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
