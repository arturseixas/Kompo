# Kompo

Editor de vídeo profissional para desktop (Windows, macOS e Linux), escrito em
Rust. Renderização em wgpu (Vulkan/Metal/DX12) com shaders WGSL e fallback em
CPU, pipeline de cor linear em ponto flutuante, decodificação por FFmpeg com
aceleração por hardware, áudio com cpal/symphonia/rubato e host OpenFX.

- Design system: [`docs/DESIGN.md`](docs/DESIGN.md)
- Decisões técnicas e licenças: [`docs/DECISIONS.md`](docs/DECISIONS.md)
- Metas de desempenho e medições: [`docs/PERFORMANCE.md`](docs/PERFORMANCE.md)
- Relatórios por fase: [`docs/phases/`](docs/phases)

## Compilar

Requisitos: Rust estável (≥ 1.90), FFmpeg ≥ 6.1 com headers, `pkg-config`,
`clang` (bindgen).

| Sistema | Dependências |
|---|---|
| Ubuntu/Debian | `sudo apt install pkg-config clang libclang-dev libavcodec-dev libavformat-dev libavutil-dev libswscale-dev libswresample-dev libavfilter-dev libavdevice-dev libasound2-dev libxkbcommon-dev libxkbcommon-x11-0` |
| macOS | `brew install ffmpeg pkgconf` |
| Windows | baixe um build *shared* **LGPL** do FFmpeg (ex.: BtbN `win64-lgpl-shared`), defina `FFMPEG_DIR` para a pasta e coloque `bin\` no `PATH` |

```sh
cargo run --release                 # abre o Kompo
cargo run --release -- clip.mp4     # abre já importando mídia
cargo test --workspace              # testes (GPU usa o adaptador disponível)
cargo run --release -p kompo-engine --bin kompo-bench -- --quick   # benchmarks
```

Variáveis úteis: `KOMPO_CPU_RENDER=1` (força o backend de render em CPU),
`KOMPO_NO_HWDEC=1` (desliga decodificação por hardware), `KOMPO_NULL_AUDIO=1`
(sem dispositivo de áudio, relógio silencioso), `WGPU_BACKEND=vulkan|metal|dx12|gl`.

## Estrutura

```
crates/
  kompo-core     modelo de timeline, tempo, keyframes, undo/redo, arquivo de projeto
  kompo-media    FFmpeg (decode/encode/hwaccel), PNG/JPEG/EXR, proxies, cache
  kompo-gpu      grafo de render: wgpu/WGSL + backend CPU
  kompo-audio    conform, mixer, DSP, medidores, saída cpal
  kompo-color    ciência de cor, LUTs, OCIO
  kompo-motion   composição em camadas
  kompo-ofx      host OpenFX
  kompo-engine   reprodução, render de timeline, exportação (headless)
  kompo-app      interface (egui)
```

## Atalhos principais

| Ação | Atalho |
|---|---|
| Play/pause · shuttle | Espaço · J / K / L |
| Quadro anterior / seguinte · edição anterior / seguinte | ← / → · ↑ / ↓ |
| Ferramentas: seleção, seleção de trilha, ripple, roll, lâmina, slip, slide | V, A, B, N, C, Y, U |
| Magnetismo | S |
| In / Out / marcar clipe / limpar | I / O / X / Ctrl+Shift+X |
| Marcador | M |
| Add Edit (destino) / em todas as trilhas | Ctrl+K / Ctrl+Shift+K |
| Apagar / Ripple Delete | Delete / Shift+Delete |
| Lift / Extract (In–Out) | ; / ' |
| Inserir / sobrescrever do media pool | , / . |
| Ângulo multicam · visão multicam | 1–9 · Shift+0 |
| Zoom da timeline | = / - / \ |
| Desfazer / refazer | Ctrl+Z / Ctrl+Shift+Z |
| Novo, abrir, salvar, salvar como, importar | Ctrl+N, Ctrl+O, Ctrl+S, Ctrl+Shift+S, Ctrl+I |
| Páginas Cut, Color, Audio, Effects, Motion | Shift+1 … Shift+5 |
