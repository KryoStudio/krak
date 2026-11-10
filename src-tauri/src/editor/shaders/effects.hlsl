// Effects drawn onto one plane of an NV12 frame before it is encoded (see editor/compose).
//
// Every draw covers the viewport (a rectangle of the plane) with a 4-vertex strip. The blend
// state does `out = src + dst * (1 - a)`: the pixel shaders return premultiplied values.
//
// - ps_stamp: a premultiplied texture of exactly the viewport's size (text: Y'·a or Cb'·a,
//   Cr'·a, and a), faded by `opacity`;
// - ps_fill: one premultiplied colour (a dip to black or white);
// - ps_pixelate: hides a rectangle behind big blocks, each the average of 4×4 samples of a
//   copy of the plane (alpha 1: the blocks replace what was there).
//
// Rebuild after editing (Windows SDK fxc, from this folder):
//   fxc /nologo /O3 /T vs_4_0 /E vs /Fo effects_vs.cso effects.hlsl
//   fxc /nologo /O3 /T ps_4_0 /E ps_stamp /Fo effects_stamp_ps.cso effects.hlsl
//   fxc /nologo /O3 /T ps_4_0 /E ps_fill /Fo effects_fill_ps.cso effects.hlsl
//   fxc /nologo /O3 /T ps_4_0 /E ps_pixelate /Fo effects_pixelate_ps.cso effects.hlsl

cbuffer Params : register(b0)
{
    float4 color;   // ps_fill: premultiplied plane values and alpha
    float opacity;  // ps_stamp
    float block;    // ps_pixelate: block side, plane pixels
    float2 origin;  // ps_pixelate: where the block grid starts, plane pixels
};

struct Vertex
{
    float4 position : SV_Position;
    float2 uv : TEXCOORD0;
};

Vertex vs(uint id : SV_VertexID)
{
    Vertex v;
    v.uv = float2(id & 1, id >> 1);
    v.position = float4(v.uv.x * 2 - 1, 1 - v.uv.y * 2, 0, 1);
    return v;
}

Texture2D<float4> stamp : register(t0);
Texture2D<float4> plane : register(t1);

float4 ps_stamp(Vertex v) : SV_Target
{
    uint width, height;
    stamp.GetDimensions(width, height);
    return stamp.Load(int3(v.uv * float2(width, height), 0)) * opacity;
}

float4 ps_fill(Vertex v) : SV_Target
{
    return color;
}

float4 ps_pixelate(Vertex v) : SV_Target
{
    float2 cell = floor((v.position.xy - origin) / block);
    float2 corner = origin + cell * block;
    float4 sum = 0;
    [unroll] for (int i = 0; i < 4; i++)
    {
        [unroll] for (int j = 0; j < 4; j++)
        {
            sum += plane.Load(int3(corner + (float2(i, j) + 0.5) * (block / 4), 0));
        }
    }
    float4 average = sum / 16;
    return float4(average.rgb, 1);
}
