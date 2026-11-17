struct Conversion { levels: vec4<f32>, matrix: vec4<f32>, rounding: vec4<f32>, sampling: vec4<f32> }
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
    var alpha = 1.0;
    if conversion.rounding.z > 0.0 {
        let size = conversion.rounding.xy;
        let radius = min(conversion.rounding.z, min(size.x, size.y) * 0.5);
        let point = vertex.uv * size;
        let corner = abs(point - size * 0.5) - (size * 0.5 - vec2(radius));
        let distance = length(max(corner, vec2(0.0))) + min(max(corner.x, corner.y), 0.0) - radius;
        alpha = 1.0 - smoothstep(-0.5, 0.5, distance);
    }
    let sample_uv = conversion.sampling.xy + vertex.uv * conversion.sampling.zw;
    let levels = conversion.levels;
    let y = (textureSample(luma, bilinear, sample_uv).r - levels.x) * levels.y;
    let uv = (textureSample(chroma, bilinear, sample_uv).rg - levels.z) * levels.w;
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
    return vec4(rgb, alpha);
}
