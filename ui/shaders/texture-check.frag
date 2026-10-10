#version 440
layout(location = 0) in vec2 qt_TexCoord0;
layout(location = 0) out vec4 fragColor;
layout(std140, binding = 0) uniform buf {
    mat4 qt_Matrix;
    float qt_Opacity;
    vec2 expected;
};
layout(binding = 1) uniform sampler2D sweep;

// Image.sourceSize is the PNG size even if Qt downsizes the GPU upload.
// A tiny readback reports whether the native dimensions survived intact.
void main() {
    ivec2 actual = textureSize(sweep, 0);
    fragColor = actual == ivec2(expected) ? vec4(0, 1, 0, 1) : vec4(1, 0, 0, 1);
}
