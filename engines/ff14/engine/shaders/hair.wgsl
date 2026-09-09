// FFXIV hair.shpk (hair, brows, lashes, tails). Reference: xivmodding.com Dawntrail shader table.
// normal: rg normal, b = highlight colour influence, a = opacity
// mask: r = specular power, g = roughness, b = SSS thickness, a = diffuse mask / ambient occlusion
// slots: tex0 = normal RGBA (linear), tex1 = mask (linear)
// params[1] = (hair r, g, b, highlights enabled), params[2] = (highlight r, g, b, has_mask),
// params[3] = material g_DiffuseColor multiplier
fn ff14_hair(in: VertexOutput, pbr: ptr<function, PbrInput>, base_in: vec4<f32>) -> vec4<f32> {
    let hair = params.data[1];
    let hi = params.data[2];
    var color = hair.rgb;
    var alpha = 1.0;
    var ao = 1.0;
    var spec = 0.3;
    var rough = 0.5;
#ifdef VERTEX_UVS_A
    let n = textureSample(tex0, samp0, in.uv);
    alpha = n.a;
    if (hair.w > 0.5) {
        color = mix(hair.rgb, hi.rgb, n.b);
    }
    if (hi.w > 0.5) {
        let m = textureSample(tex1, samp1, in.uv);
        ao = m.a;
        spec = m.r;
        rough = m.g;
    }
#endif
    // The mask alpha is the strand diffuse mask (squared, as the game does): it carries all of
    // the hair's texture. g_DiffuseColor (1.4 on retail hair) lifts the result.
    color = color * params.data[3].rgb * ao * ao;
    (*pbr).material.perceptual_roughness = clamp(0.4 + 0.55 * rough, 0.3, 0.95);
    (*pbr).material.reflectance = vec3<f32>(0.08 + 0.3 * spec);
    return vec4<f32>(color, alpha);
}
