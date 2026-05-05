fn main() {
    let pe = "\\begin{align*}\nPE_{(pos,2i)} = sin(pos / 10000^{2i/d_{model}}) \\\\\nPE_{(pos,2i+1)} = cos(pos / 10000^{2i/d_{model}})\n\\end{align*}";
    println!("=== PE source ===");
    println!("{}", pe);
    println!("=== render output ===");
    let out = math_render::render(math_render::MathInput::Latex(pe));
    println!("{}", out);
    println!("=== repr ===");
    println!("{:?}", out);
    println!();

    let lrate = "\\begin{equation}\nlrate = d^{-0.5} \\cdot \\\\\n  \\min(x^{-0.5}, y \\cdot z^{-1.5})\n\\end{equation}";
    println!("=== lrate source ===");
    println!("{}", lrate);
    println!("=== render output ===");
    let out2 = math_render::render(math_render::MathInput::Latex(lrate));
    println!("{}", out2);
    println!("=== repr ===");
    println!("{:?}", out2);
}
