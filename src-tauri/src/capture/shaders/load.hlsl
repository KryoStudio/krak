// A pixel shader as heavy as a demanding game's, for `capture::tests::gpu_load_helper`: it keeps
// the GPU's shader cores and 3D engine busy from another process while the engine records.
// Compiled with the Windows SDK's fxc:
//   fxc /nologo /O3 /T vs_4_0 /E vs /Fo load_vs.cso load.hlsl
//   fxc /nologo /O3 /T ps_4_0 /E ps /Fo load_ps.cso load.hlsl

cbuffer Load : register(b0)
{
    uint iterations;
    float seed;
    float2 unused;
};

// One triangle covering the target.
float4 vs(uint id : SV_VertexID) : SV_Position
{
    float2 uv = float2((id << 1) & 2, id & 2);
    return float4(uv * float2(2, -2) + float2(-1, 1), 0, 1);
}

// Shader-core bound arithmetic, different every frame (`seed`) so the screen keeps changing.
float4 ps(float4 position : SV_Position) : SV_Target
{
    float3 v = float3(position.xy * 0.001, seed);
    [loop]
    for (uint i = 0; i < iterations; i++)
    {
        v = frac(sin(v * 12.9898 + v.yzx * 78.233) * 43758.5453 + v.zxy);
    }
    return float4(v, 1);
}
