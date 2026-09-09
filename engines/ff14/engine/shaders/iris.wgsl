// FFXIV iris.shpk. mask: r = emissive, g = reflection, b = iris mask. Vertex colour r/g = left/right.
// slots: tex0 = mask (linear)
// params[1] = (left eye r,g,b, has_mask), params[2] = (right eye r,g,b, 0)
fn ff14_iris(in: VertexOutput, pbr: ptr<function, PbrInput>, base_in: vec4<f32>) -> vec4<f32> {
    let left = params.data[1];
    let right = params.data[2];
    var eye = left.rgb;
#ifdef VERTEX_COLORS
    eye = mix(right.rgb, left.rgb, clamp(in.color.r, 0.0, 1.0));
#endif
    var iris_mask = 1.0;
    var emissive = 0.0;
    var reflect = 0.5;
#ifdef VERTEX_UVS_A
    if (left.w > 0.5) {
        let m = textureSample(tex0, samp0, in.uv);
        iris_mask = m.b;
        emissive = m.r;
        reflect = m.g;
    }
#endif
    let color = mix(base_in.rgb, base_in.rgb * eye * 1.3, iris_mask);
    (*pbr).material.perceptual_roughness = 0.15;
    (*pbr).material.reflectance = vec3<f32>(0.3 + 0.4 * reflect);
    (*pbr).material.emissive = vec4<f32>(color * emissive * 0.5, 1.0);
    return vec4<f32>(color, base_in.a);
}
