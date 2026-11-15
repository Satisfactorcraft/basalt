#version 450

// Per-frame data shared by every draw.
layout(set = 0, binding = 0) uniform FrameData {
    mat4 view_proj;
    vec4 camera_pos;   // xyz = camera world position
    vec4 sun_dir;      // xyz = direction the light travels (normalized)
    vec4 sun_color;    // rgb = color * intensity
    vec4 ambient;      // rgb = ambient color
} frame;

// Per-draw data.
layout(push_constant) uniform DrawData {
    mat4 model;
    vec4 base_color;
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
    out_normal = mat3(transpose(inverse(draw.model))) * in_normal;
    out_uv = in_uv;
    gl_Position = frame.view_proj * world;
}
