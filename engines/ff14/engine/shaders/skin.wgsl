// FFXIV skin.shpk (Dawntrail). Reference: xivmodding.com Dawntrail shader table.
// diffuse: rgb colour, a opacity (face: lip mask)
// normal: rg normal, b = skin colour influence, a = tile mask
// mask: r = specular power, g = roughness, b = SSS thickness, a = hair highlight influence (fur)
// Faces: normal.a is the lip mask (the diffuse alpha stays opacity).
// slots: tex0 = normal RGBA (linear), tex1 = mask (linear), tex2 = face paint decal (BC4, r)
// params[1] = (skin r, g, b, has_mask), params[2] = (lip r, g, b, lip opacity; 0 = not a face),
// params[3] = (hair r, g, b, has_normal), params[4] = material g_DiffuseColor (tint where the skin
// influence is 0, e.g. teeth), params[5] = (decal r, g, b, opacity), params[6] = (decal uv
// scale, decal uv offset, has_decal, 0): the decal is a front projection through the face's
// second UV set (`FacePaintUvMultiplier` / `Offset` of the game's customize parameters).
fn ff14_skin(in: VertexOutput, pbr: ptr<function, PbrInput>, base_in: vec4<f32>) -> vec4<f32> {
    let skin = params.data[1];
    let lip = params.data[2];
    let hair = params.data[3];
    var color = base_in.rgb;
    var alpha = 1.0;
    var influence = 1.0;
    var lip_mask = 0.0;
    var sss = 0.35;
    var spec = 0.25;
    var rough = 0.6;
#ifdef VERTEX_UVS_A
    if (hair.w > 0.5) {
        let n = textureSample(tex0, samp0, in.uv);
        influence = n.b;
        lip_mask = n.a;
    }
    if (skin.w > 0.5) {
        let m = textureSample(tex1, samp1, in.uv);
        spec = m.r;
        rough = m.g;
        sss = m.b;
        // Fur/facial hair regions take the hair colour.
        color = mix(color, color * hair.rgb, clamp(m.a, 0.0, 1.0));
    }
#endif
    // Skin tone (squared RGB, i.e. linear) multiplies the linear base where the influence mask
    // says so. Subsurface thickness only softens the lighting response here.
    let dc = params.data[4].rgb;
    var shaded = color * mix(dc, skin.rgb, influence);
    alpha = base_in.a;
    if (lip.w > 0.0) {
        shaded = mix(shaded, lip.rgb, clamp(lip_mask, 0.0, 1.0) * lip.w);
    }
#ifdef VERTEX_UVS_B
    let decal = params.data[5];
    let decal_uv = params.data[6];
    var paint = 0.0;
    if (decal_uv.z > 0.5) {
        let duv = in.uv_b * decal_uv.x + vec2<f32>(decal_uv.y);
        let inside = step(0.0, duv.x) * step(duv.x, 1.0) * step(0.0, duv.y) * step(duv.y, 1.0);
        paint = textureSample(tex2, samp2, clamp(duv, vec2<f32>(0.0), vec2<f32>(1.0))).r * inside;
        shaded = mix(shaded, decal.rgb, clamp(paint * decal.w, 0.0, 1.0));
    }
    if (params.data[0].y > 15.5 && params.data[0].y < 16.5) {
        (*pbr).material.emissive = vec4<f32>(paint * 4.0, fract(in.uv_b.x * 8.0) * 0.5, fract(in.uv_b.y * 8.0) * 0.5, 0.0);
        (*pbr).material.reflectance = vec3<f32>(0.0);
        (*pbr).material.perceptual_roughness = 1.0;
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }
#endif
    (*pbr).material.perceptual_roughness = clamp(0.45 + 0.5 * rough, 0.3, 0.95);
    (*pbr).material.reflectance = vec3<f32>(0.1 + 0.3 * spec);
    // Debug views (FFL_SHADER_DEBUG, unlit through emissive — a single grey per view reads
    // back through the tonemapper, separate channels do not): 11 base texture, 12 skin
    // colour influence, 13 lip mask, 14 mask rgb, 15 the shaded colour, 16 (faces) decal × 4
    // in r over a grid of the decal uv (8 cells) in g/b.
    let dbg = params.data[0].y;
    if (dbg > 10.5 && dbg < 15.5) {
        var v = base_in.rgb;
        if (dbg > 11.5 && dbg < 12.5) { v = vec3<f32>(influence); }
        if (dbg > 12.5 && dbg < 13.5) { v = vec3<f32>(lip_mask); }
        if (dbg > 13.5 && dbg < 14.5) { v = vec3<f32>(spec, rough, sss); }
        if (dbg > 14.5) { v = shaded; }
        (*pbr).material.emissive = vec4<f32>(v, 0.0);
        (*pbr).material.reflectance = vec3<f32>(0.0);
        (*pbr).material.perceptual_roughness = 1.0;
        (*pbr).material.metallic = 0.0;
        return vec4<f32>(0.0, 0.0, 0.0, alpha);
    }
    return vec4<f32>(shaded, alpha);
}
