fn main() {
    for e in filmcraft_audio_dsp::effects() {
        println!("{:?} | {} | {} | {}", e.category, e.id, e.name, e.params.iter().map(|p| p.id).collect::<Vec<_>>().join(","));
    }
}
