// FFXIV character.shpk / characterlegacy.shpk (gear, weapons). Reference: xivmodding.com table,
// Meddle MaterialUtility (v0.1.20).
// normal: rg normal, b = opacity (cutout when g_AlphaThreshold > 0)
// mask: r = specular scale, g = roughness (Dawntrail) / gloss (legacy, inverted), b = ambient occlusion
// index: r = colour-set row pair (0-15 over 32 rows), g = blend between the pair: 255 = the
// A row (even, the lit colour), 0 = the B row (odd, its shaded version) — DTM's id-map
// convention, verified in the game by its colour-set decals.
// slots: tex0 = index (linear), tex1 = mask (linear), tex2 = normal RGBA (linear),
// tex3 = scrolling effect (characterscroll.shpk, linear)
// params[1] = (row count, has_index, has_mask, legacy), params[98] = (has_normal, alpha_from_normal, has_effect, 0),
// params[99] = (effect tiling u, v, scroll u, v per second)
// params[2..34) rows: rgb diffuse, a roughness (legacy: gloss strength);
// [34..66) rgb specular colour, a metalness (legacy: 0);
// [66..98) rgb emissive, a 1 (legacy: specular power, Phong-like 3..20)
fn ff14_character(in: VertexOutput, pbr: ptr<function, PbrInput>, base_in: vec4<f32>) -> vec4<f32> {
    var base = base_in;
#ifdef VERTEX_UVS_A
    let p = params.data[1];
    let q = params.data[98];
    let n_rows = p.x;
    let legacy = p.w > 0.5;
    var rough = 0.7;
    var metal = 0.0;
    var refl = vec3<f32>(0.25);
    var spec_color = vec3<f32>(0.5);
    var spec_power = 8.0;
    var gloss_strength = 1.0;
    var row_rough = 0.5;
    var row_metal = 0.0;
    var diff_luma = 0.5;
    var have_rows = false;
    if (p.y > 0.5 && n_rows > 0.5) {
        let idx = textureSample(tex0, samp0, in.uv);
        var a: u32;
        var b: u32;
        var t: f32;
        if (n_rows > 31.5) {
            let pair = u32(clamp(round(idx.r * 15.0), 0.0, 15.0));
            a = pair * 2u;
            b = min(a + 1u, 31u);
            t = idx.g;
        } else {
            a = u32(clamp(round(idx.r * (n_rows - 1.0)), 0.0, n_rows - 1.0));
            b = a;
            t = 0.0;
        }
        let ra = mix(params.data[2u + b], params.data[2u + a], t);
        let rb = mix(params.data[34u + b], params.data[34u + a], t);
        let rc = mix(params.data[66u + b], params.data[66u + a], t);
        base = vec4<f32>(base.rgb * ra.rgb, base.a);
        spec_color = rb.rgb;
        diff_luma = dot(ra.rgb, vec3<f32>(0.299, 0.587, 0.114));
        have_rows = true;
        if (legacy) {
            gloss_strength = ra.a;
            spec_power = max(rc.a, 1.0);
        } else {
            row_rough = select(0.5, ra.a, ra.a > 0.0);
            row_metal = rb.a;
        }
        // Alpha 0: display units (Bevy scales alpha-1 emissive by the camera exposure, which
        // is tiny with the physical sun this scene uses).
        var glow = rc.rgb;
        if (q.z > 0.5) {
            // characterscroll: the row's emissive only where the scrolling effect is bright.
            let e = params.data[99];
            let euv = in.uv * e.xy + globals.time * e.zw;
            let f = textureSample(tex3, samp3, euv).r;
            glow = rc.rgb * f * f;
        }
        (*pbr).material.emissive = vec4<f32>(glow * 0.5, 0.0);
    }
    // Mask: r specular scale, g roughness (Dawntrail) / gloss (legacy), b multiplies the
    // diffuse (proven in the game: b = 0 renders black).
    var m = vec4<f32>(0.5, 0.5, 1.0, 1.0);
    if (p.z > 0.5) {
        m = textureSample(tex1, samp1, in.uv);
        base = vec4<f32>(base.rgb * m.b, base.a);
    }
    if (legacy) {
        // Phong power n (3..20 on retail rows) modulated by the gloss map, then Beckmann-ish
        // alpha = sqrt(2 / (n + 2)) and perceptual roughness = sqrt(alpha): n = 3.5 gives 0.78,
        // n = 20 gives 0.55. Treating alpha itself as the perceptual value made everything glossy.
        let n = spec_power * (0.5 + m.g);
        rough = clamp(pow(2.0 / (n + 2.0), 0.25), 0.2, 0.95);
        // Legacy rows have no metalness. A specular colour brighter than the diffuse reads as
        // metal only together with a tight highlight (high power): dark leather with a bright,
        // broad (n ≈ 3) specular is sheen, not metal.
        let spec_luma = dot(spec_color, vec3<f32>(0.299, 0.587, 0.114)) * gloss_strength;
        metal = clamp((spec_luma - diff_luma) * 2.0, 0.0, 1.0) * smoothstep(8.0, 16.0, spec_power);
        base = vec4<f32>(mix(base.rgb, spec_color, metal * 0.7), base.a);
        refl = clamp(spec_color * gloss_strength * (0.2 + 0.5 * m.r), vec3<f32>(0.0), vec3<f32>(0.8));
    } else {
        // Row roughness 0.5 is neutral; the mask carries the detail. Specular rows are white
        // on nearly everything, so scale to the dielectric range (0.5 = 4% F0) by the mask.
        rough = clamp(row_rough * 2.0 * m.g, 0.2, 1.0);
        metal = row_metal;
        refl = clamp(spec_color * (0.1 + 0.5 * m.r), vec3<f32>(0.0), vec3<f32>(0.8));
    }
    if (!have_rows) {
        rough = clamp(0.4 + 0.6 * (1.0 - m.g), 0.3, 0.95);
        refl = vec3<f32>(0.25 + 0.25 * m.r);
    }
    if (q.x > 0.5 && q.y > 0.5) {
        let n = textureSample(tex2, samp2, in.uv);
        base = vec4<f32>(base.rgb, n.b);
    }
    // Debug views (FFL_SHADER_DEBUG): 1 diffuse only, 2 no emissive, 3 no metal/specular colour.
    let dbg = params.data[0].y;
    if (dbg > 0.5) {
        (*pbr).material.emissive = vec4<f32>(0.0, 0.0, 0.0, 0.0);
        if (dbg < 1.5) {
            rough = 0.7;
            metal = 0.0;
            refl = vec3<f32>(0.25);
        } else if (dbg > 2.5) {
            metal = 0.0;
            refl = vec3<f32>(0.25);
        }
    }
    (*pbr).material.perceptual_roughness = clamp(rough, 0.05, 1.0);
    (*pbr).material.metallic = clamp(metal, 0.0, 1.0);
    (*pbr).material.reflectance = clamp(refl, vec3<f32>(0.0), vec3<f32>(1.0));
#endif
    return base;
}
