# Kompo como host OpenFX

`kompo-ofx` implementa um host **OpenFX 1.4** (Open Effects Association) em Rust: descobre bundles, carrega os
binários `.ofx` com `libloading`, entrega as suítes que o plug-in busca com `fetchSuite` e renderiza instâncias em
buffers de CPU. O efeito entra na timeline como qualquer outro (`ofx:<identificador>` em `clip.video.effects`), com
parâmetros animáveis por keyframes, e aparece na biblioteca da página Effects sob "OpenFX".

## O que está implementado

| Parte | Estado |
|---|---|
| Descoberta em pastas padrão | Linux `/usr/OFX/Plugins`, `/usr/local/OFX/Plugins`, `~/.OFX/Plugins`; macOS `/Library/OFX/Plugins`, `~/Library/OFX/Plugins`; Windows `%CommonProgramFiles%\OFX\Plugins`; mais `OFX_PLUGIN_PATH` e `KOMPO_OFX_PATH` |
| Bundles `.ofx.bundle` | `Contents/<Linux-x86-64 \| Linux-aarch64 \| MacOS \| Win64>/X.ofx`, e `.ofx` soltos |
| Ações | Load, Describe, DescribeInContext, CreateInstance, InstanceChanged, BeginSequenceRender, Render, EndSequenceRender, DestroyInstance |
| Contextos | Filter, Generator, General |
| Suítes | Property, Parameter, ImageEffect, Memory, MultiThread, Message v1/v2, Progress, TimeLine |
| Parâmetros | Integer, Double, Boolean, Choice, RGB, RGBA, 2D/3D (double e int), String, Custom; Group/Page/PushButton são lidos mas não viram controles. Coordenadas normalizadas → canônicas (pixels) |
| Clips | Source, Output e extras (não conectados); imagens em float, 16 e 8 bits RGBA pré-multiplicado, linhas de baixo para cima como manda a especificação |
| Animação | os parâmetros são keyframes do Kompo; `paramGetValueAtTime`, `GetNumKeys`, `GetKeyTime`, `GetKeyIndex` os leem |
| ABI variádica | `paramGetValue`/`SetValue` (variádicas em C, impossíveis de definir em Rust estável) espelham o registro/pilha de cada ABI: System V x86-64 e AArch64 Linux, Win64 e Apple arm64 |

## O que não está

- **Renderização por GPU (OpenGL/CUDA/Metal)**: o host não oferece essas suítes; plug-ins que as têm opcionais
  rodam em CPU. Plug-ins que *exigem* GPU não carregam. (`PluginInfo::opengl_capable` indica quais seriam.)
- Interacts/overlays no viewer, parâmetros paramétricos, contextos Transition/Retimer/Paint, acesso temporal a clips.
- As cores são entregues no espaço de trabalho linear do Kompo (sem conversão para o espaço do plug-in).

## Plug-ins de teste (projeto OpenFX, BSD-3-Clause)

`scripts/build-ofx-examples.sh` clona https://github.com/AcademySoftwareFoundation/openfx (commit
`adb790bf0043714cae6091d58791e8cdabdd9e12` quando testado), compila os exemplos para `target/ofx-examples`, e
`KOMPO_OFX_TEST_DIR=target/ofx-examples cargo test -p kompo-ofx` os exercita. `cargo run -p kompo-ofx --example ofx_report -- <pasta>`
imprime a tabela abaixo.

| Plug-in (identificador) | Origem | Contexto | Resultado |
|---|---|---|---|
| `uk.co.thefoundry.OfxInvertExample` | Examples/Invert | Filter | **passou** — inverte e é involução (testado) |
| `net.sf.openfx.invertPlugin` | Support/Plugins/Invert (C++ support lib) | Filter | **passou** |
| `org.openfx.ChoiceParamPlugin` | Examples/ChoiceParams | Filter | **passou** — opções do Choice chegam à UI, valores mudam a imagem (testado) |
| `uk.co.thefoundry.BasicGainPlugin` | Examples/Basic | Filter | **passou** — `scale=0.5` escala o RGB (testado) |
| `uk.co.thefoundry.GeneratorExample` (Rectangle) | Examples/Rectangle | Filter | **passou** — corners normalizados viram pixels; cor RGBA aplicada (testado) |
| `uk.co.thefoundry.DepthConverterExample` | Examples/DepthConverter | Filter | **passou** |
| `net.sf.openfx.fieldPlugin` | Support/Plugins/Field | Filter | **passou** |
| `net.sf.openfx.noisePlugin` | Support/Plugins/Generator | Generator | **passou** — gera pixels variados (testado) |
| `net.sf.openfx.crossFade` | Support/Plugins/Transition | General | **passou** (renderiza como filtro sobre o Source) |
| `net.sf.openfx.retimer` | Support/Plugins/Retimer | — | **não suportado**: exige acesso temporal a clips (recusado com `ErrMissingHostFeature`) |

Os exemplos `Basic`/`ChoiceParams`/`Tester` de `Support/Plugins` e `OpenGL`/`Custom`/`DrawSuite`/`ColourSpace` não foram
compilados: dependem de OpenGL, CImg ou spdlog (ver o script). Para testar plug-ins comerciais de terceiros,
aponte `KOMPO_OFX_PATH` para a pasta deles.
