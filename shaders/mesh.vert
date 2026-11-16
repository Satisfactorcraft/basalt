#version 450

// Per-frame data shared by every draw.
layout(set = 0, binding = 0) uniform FrameData {
    mat4 view_proj;
    vec4 camera_pos;   // xyz = camera world position
    vec4 sun_dir;      // xyz = direction the light travels (normalized)
    vec4 sun_color;    // rgb = color * intensity
    vec4 ambient;      // rgb = ambient color
    vec4 params;       // x = 1: gamma-encode in the shader (non-sRGB swapchain)
} frame;

// Per-draw data.
layout(push_constant) uniform DrawData {
    mat4 model;
    vec4 base_color;
    vec4 material;     // x = alpha cutoff (< 0: opaque, alpha ignored)
} draw;

layout(location = 0) in vec3 in_position;
layout(location = 1) in vec3 in_normal;
layout(location = 2) in vec2 in_uv;

layout(location = 0) out vec3 out_world_pos;
layout(location = 1) out vec3 out_normal;
layout(location = 2) out vec2 out_uv;

void main() {
    vec4 world = draw.model * vec4(in_position, 1.0);
    out_world_pos = world.xyz;
    // Normal matrix via the cofactor matrix: same direction as
    // transpose(inverse(M)) but well defined when M is singular (a zero
    // scale on some axis), where inverse() produces NaN. Its sign flips with
    // det(M), so undo that for mirrored transforms.
    mat3 m = mat3(draw.model);
    mat3 cofactor = mat3(cross(m[1], m[2]), cross(m[2], m[0]), cross(m[0], m[1]));
    float det = dot(m[0], cofactor[0]);
    out_normal = (det < 0.0 ? -1.0 : 1.0) * (cofactor * in_normal);
    out_uv = in_uv;
    gl_Position = frame.view_proj * world;
}
