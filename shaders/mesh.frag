#version 450

layout(set = 0, binding = 0) uniform FrameData {
    mat4 view_proj;
    vec4 camera_pos;
    vec4 sun_dir;
    vec4 sun_color;
    vec4 ambient;
    vec4 params;
} frame;

layout(set = 1, binding = 0) uniform sampler2D base_color_tex;

layout(push_constant) uniform DrawData {
    mat4 model;
    vec4 base_color;
    vec4 material;     // x = alpha cutoff (< 0: opaque, alpha ignored)
} draw;

layout(location = 0) in vec3 in_world_pos;
layout(location = 1) in vec3 in_normal;
layout(location = 2) in vec2 in_uv;

layout(location = 0) out vec4 out_color;

void main() {
    vec4 albedo = texture(base_color_tex, in_uv) * draw.base_color;
    // glTF alphaMode: OPAQUE ignores alpha, MASK discards below the cutoff.
    // BLEND isn't supported yet and is loaded as MASK.
    float cutoff = draw.material.x;
    if (cutoff >= 0.0 && albedo.a < cutoff) {
        discard;
    }

    // Faces flattened to zero area can still carry a zero normal; avoid NaN.
    float len = length(in_normal);
    vec3 n = len > 1e-12 ? in_normal / len : vec3(0.0, 1.0, 0.0);
    if (!gl_FrontFacing) {
        n = -n; // back face of a double-sided material
    }
    vec3 l = normalize(-frame.sun_dir.xyz);
    vec3 v = normalize(frame.camera_pos.xyz - in_world_pos);
    vec3 h = normalize(l + v);

    float diffuse = max(dot(n, l), 0.0);
    float specular = diffuse > 0.0 ? pow(max(dot(n, h), 0.0), 32.0) * 0.25 : 0.0;

    vec3 color = albedo.rgb * (frame.ambient.rgb + frame.sun_color.rgb * diffuse)
               + frame.sun_color.rgb * specular;

    // Normally the swapchain is sRGB and the hardware gamma-encodes. If only
    // a UNORM format was available, apply the sRGB curve here instead.
    if (frame.params.x > 0.5) {
        vec3 c = clamp(color, 0.0, 1.0);
        color = mix(c * 12.92, 1.055 * pow(c, vec3(1.0 / 2.4)) - 0.055, step(0.0031308, c));
    }
    out_color = vec4(color, 1.0);
}
