// FFXI zone surfaces: texture × vertex colour (0x80 = 1.0, already normalised on the CPU).
// FFL_SHADER_DEBUG: 1 = uv as colour, 2 = texture only, 3 = vertex colour only.
fn ff11_zone(in: VertexOutput, pbr: ptr<function, PbrInput>, base: vec4<f32>) -> vec4<f32> {
    var c = base;
    let dbg = params.data[0].y;
#ifdef VERTEX_COLORS
    c = vec4<f32>(base.rgb * in.color.rgb, base.a * clamp(in.color.a, 0.0, 1.0));
    if (dbg > 2.5) {
        c = vec4<f32>(in.color.rgb, 1.0);
    }
#endif
    if (dbg > 1.5 && dbg < 2.5) {
        c = vec4<f32>(base.rgb, 1.0);
    }
#ifdef VERTEX_UVS_A
    if (dbg > 0.5 && dbg < 1.5) {
        c = vec4<f32>(fract(in.uv.x), fract(in.uv.y), 0.0, 1.0);
    }
#endif
    // Translucent cutouts (params[1].x = threshold): the client alpha tests these as well.
    let cut = params.data[1].x;
    if (cut > 0.0 && c.a < cut) {
        discard;
    }
    (*pbr).material.perceptual_roughness = 0.9;
    (*pbr).material.metallic = 0.0;
    (*pbr).material.reflectance = vec3<f32>(0.15);
    return c;
}
