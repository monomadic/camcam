struct Conversion { levels: vec4<f32>, matrix: vec4<f32> }
@group(0) @binding(0) var luma: texture_2d<f32>;
@group(0) @binding(1) var chroma: texture_2d<f32>;
@group(0) @binding(2) var bilinear: sampler;
@group(0) @binding(3) var<uniform> conversion: Conversion;

struct Vertex { @builtin(position) position: vec4<f32>, @location(0) uv: vec2<f32> }
@vertex fn vertex_main(@builtin(vertex_index) index: u32) -> Vertex {
    let positions = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    let p = positions[index];
    var result: Vertex;
    result.position = vec4(p, 0.0, 1.0);
    result.uv = vec2(p.x * 0.5 + 0.5, 0.5 - p.y * 0.5);
    return result;
}
@fragment fn fragment_main(vertex: Vertex) -> @location(0) vec4<f32> {
    let levels = conversion.levels;
    let y = (textureSample(luma, bilinear, vertex.uv).r - levels.x) * levels.y;
    let uv = (textureSample(chroma, bilinear, vertex.uv).rg - levels.z) * levels.w;
    let kr = conversion.matrix.x;
    let kb = conversion.matrix.y;
    let kg = 1.0 - kr - kb;
    var rgb = vec3(y + (2.0 - 2.0 * kr) * uv.y,
                   y - kb * (2.0 - 2.0 * kb) / kg * uv.x - kr * (2.0 - 2.0 * kr) / kg * uv.y,
                   y + (2.0 - 2.0 * kb) * uv.x);
    rgb = clamp(rgb, vec3(0.0), vec3(1.0));
    if conversion.matrix.z > 0.5 {
        rgb = select(pow((rgb + 0.055) / 1.055, vec3(2.4)), rgb / 12.92, rgb <= vec3(0.04045));
    }
    return vec4(rgb, 1.0);
}
