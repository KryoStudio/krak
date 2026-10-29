// Draws the "by krak" badge onto one plane of an NV12 frame (see editor/watermark.rs).
//
// The badge texture already holds premultiplied studio-range values for that plane (luma:
// Y'*a; chroma: Cb'*a, Cr'*a) and the alpha, at exactly the viewport's size, so each pixel
// loads its own texel; the blend state does `out = src + dst * (1 - a)`.
//
// Rebuild after editing (Windows SDK fxc, from this folder):
//   fxc /nologo /O3 /T vs_4_0 /E vs /Fo badge_vs.cso badge.hlsl
//   fxc /nologo /O3 /T ps_4_0 /E ps /Fo badge_ps.cso badge.hlsl

struct Vertex
{
    float4 position : SV_Position;
    float2 uv : TEXCOORD0;
};

// A quad covering the viewport, as a 4-vertex triangle strip without vertex buffer.
Vertex vs(uint id : SV_VertexID)
{
    Vertex v;
    v.uv = float2(id & 1, id >> 1);
    v.position = float4(v.uv.x * 2 - 1, 1 - v.uv.y * 2, 0, 1);
    return v;
}

Texture2D<float4> badge : register(t0);

float4 ps(Vertex v) : SV_Target
{
    uint width, height;
    badge.GetDimensions(width, height);
    return badge.Load(int3(v.uv * float2(width, height), 0));
}
