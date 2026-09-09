// FFXIV background (bg.shpk family): diffuse (+ second layer by vertex colour alpha), specular map.
// The second layer is mapped by the second UV set when the mesh has one: Mist's stair walls
// collapse uv0 on the triangles that show plaster and map them through uv1, so sampling the
// plaster with uv0 painted them as flat single-texel patches.
// slots: tex0 = second diffuse (sRGB), tex1 = specular (linear), tex2/tex3 = layer normal maps
// (linear; blue = height). params[1] = (two_layer, has_specular, has_diffuse, has_heights)
// The blend is sharpened around the vertex alpha with the height difference of the two
// layers, the way the game's layers meet along texture detail: a linear mix of the vertex
// alpha left every plaster triangle as a soft flat gradient.
fn ff14_bg(in: VertexOutput, pbr: ptr<function, PbrInput>, base_in: vec4<f32>) -> vec4<f32> {
    var base = base_in;
    let p = params.data[1];
#ifdef VERTEX_UVS_A
    if (p.z > 0.5) {
        base = textureSample(pbr_bindings::base_color_texture, pbr_bindings::base_color_sampler, in.uv);
    }
#ifdef VERTEX_COLORS
    if (p.x > 0.5) {
        var uv2 = in.uv;
#ifdef VERTEX_UVS_B
        uv2 = in.uv_b;
#endif
        let layer2 = textureSample(tex0, samp0, uv2);
        var w = in.color.a;
        if (p.w > 0.5) {
            let h0 = textureSample(tex2, samp2, in.uv).b;
            let h1 = textureSample(tex3, samp3, uv2).b;
            w = clamp((in.color.a - 0.5 + (h1 - h0) * 0.5) * 4.0 + 0.5, 0.0, 1.0);
        }
        base = vec4<f32>(mix(base.rgb, layer2.rgb, w), base.a);
    }
#endif
    if (p.y > 0.5) {
        let spec = textureSample(tex1, samp1, in.uv).r;
        (*pbr).material.perceptual_roughness = clamp(1.0 - 0.6 * spec, 0.15, 1.0);
        (*pbr).material.reflectance = vec3<f32>(0.08 + 0.45 * spec);
    }
#endif
    return base;
}
