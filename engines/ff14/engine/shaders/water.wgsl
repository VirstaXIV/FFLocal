// FFXIV water (water.shpk seas/lakes, river.shpk flowing water): two scrolling wave normal
// maps bend the flat surface, the view angle decides how much sky is reflected versus how
// much of the bed shows through (Fresnel), whitecaps add foam.
// slots: tex0 = wave normal A, tex1 = wave normal B, tex2 = whitecap mask, tex3 = wavelet
// params[1] = surface colour (linear) + alpha, params[2] = deep colour + river flag,
// params[3] = (wave tile metres, scroll speed, normal strength, foam amount)
fn ff14_water(in: VertexOutput, pbr: ptr<function, PbrInput>, base_in: vec4<f32>) -> vec4<f32> {
    let p1 = params.data[1];
    let p2 = params.data[2];
    let p3 = params.data[3];
    let t = globals.time;
    let river = p2.w > 0.5;
    var uv = in.world_position.xz / max(p3.x, 0.1);
#ifdef VERTEX_UVS_A
    if (river) {
        uv = in.uv * 3.0;
    }
#endif
    var flow = vec2<f32>(0.03, 0.02);
    if (river) {
        flow = vec2<f32>(0.0, -0.25);
    }
    let uv_a = uv + flow * t * p3.y;
    let uv_b = uv * 1.73 + (flow.yx * vec2<f32>(-1.0, 1.0) + vec2<f32>(0.01, 0.0)) * t * p3.y;
    // Tangent-space normals (x right, y forward, z up) of a horizontal sheet.
    let na = textureSample(tex0, samp0, uv_a).xyz * 2.0 - 1.0;
    let nb = textureSample(tex1, samp1, uv_b).xyz * 2.0 - 1.0;
    let strength = p3.z;
    let bend = (na.xy + nb.xy) * strength;
    // The sheet's own normal (flipped by Bevy for back faces, i.e. when seen from below).
    let up = (*pbr).world_normal;
    let flat = normalize(up);
    // Bend along world X/Z; the ripple is symmetric so the same delta serves both faces.
    var n = normalize(flat + vec3<f32>(bend.x, 0.0, bend.y));
    (*pbr).N = n;
    (*pbr).world_normal = n;
    let v = (*pbr).V;
    let ndv = clamp(dot(n, v), 0.0, 1.0);
    let fresnel = pow(1.0 - ndv, 5.0);
    // Looking straight down shows the bed through the surface colour; at grazing angles the
    // surface closes up and mirrors the sky.
    var alpha = mix(p1.w, 1.0, fresnel);
    let cap = textureSample(tex2, samp2, uv * 0.5 + vec2<f32>(0.012, -0.017) * t * p3.y).r;
    let foam = smoothstep(0.62, 0.95, cap) * p3.w;
    let colour = mix(p2.rgb, p1.rgb, 0.35 + 0.65 * ndv);
    let c = mix(colour, vec3<f32>(0.9, 0.95, 1.0), foam);
    alpha = max(alpha, foam);
#ifdef VERTEX_COLORS
    // Vertex alpha fades a sheet out: Mist's near-shore wavelet layer sits over the sea at
    // 0.004, leaving only its foam; drawing it opaque doubled the water along the beach.
    alpha = alpha * clamp(in.color.a, 0.0, 1.0);
#endif
    (*pbr).material.perceptual_roughness = 0.08;
    (*pbr).material.metallic = 0.0;
    (*pbr).material.reflectance = vec3<f32>(0.35);
    return vec4<f32>(c, alpha);
}
