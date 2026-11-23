in vec3 color;
in float opacity;
out vec4 frag;
void main(){vec2 q=gl_PointCoord*2.0-1.0;float r=dot(q,q);if(r>1.0)discard;float a=exp(-r*2.2)*opacity;frag=vec4(color,a);}
