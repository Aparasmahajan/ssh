// One pipeline for the whole terminal: solid rectangles and atlas glyphs,
// distinguished per instance. One draw call per frame (CLAUDE.md §5.7).

struct Uniforms {
    viewport: vec2<f32>,
    _pad: vec2<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var atlas: texture_2d<f32>;

struct In {
    @builtin(vertex_index) vi: u32,
    @location(0) pos: vec2<f32>,
    @location(1) size: vec2<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) color: u32,
    @location(4) kind: u32,
};

struct Out {
    @builtin(position) clip: vec4<f32>,
    @location(0) local: vec2<f32>,
    @location(1) @interpolate(flat) uv: vec2<f32>,
    @location(2) @interpolate(flat) color: vec4<f32>,
    @location(3) @interpolate(flat) kind: u32,
};

@vertex
fn vs(i: In) -> Out {
    // Triangle strip: (0,0) (1,0) (0,1) (1,1).
    let corner = vec2<f32>(f32(i.vi & 1u), f32(i.vi >> 1u));
    let p = i.pos + corner * i.size;
    var o: Out;
    o.clip = vec4<f32>(p.x / u.viewport.x * 2.0 - 1.0, 1.0 - p.y / u.viewport.y * 2.0, 0.0, 1.0);
    o.local = corner * i.size;
    o.uv = i.uv;
    o.color = unpack4x8unorm(i.color);
    o.kind = i.kind;
    return o;
}

@fragment
fn fs(v: Out) -> @location(0) vec4<f32> {
    if v.kind == 0u {
        return v.color;
    }
    // Quads are pixel-aligned, so this pixel's texel is exact: no sampler, no
    // filtering, nothing bleeding in from the neighbouring glyph.
    let texel = vec2<i32>(v.uv + floor(v.local));
    let coverage = textureLoad(atlas, texel, 0).r;
    return vec4<f32>(v.color.rgb, v.color.a * coverage);
}
