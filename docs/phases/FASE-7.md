# Fase 7 — Host OpenFX

## O que foi implementado
Host OpenFX 1.4 completo para filtros/geradores em CPU — ver [`docs/OFX.md`](../OFX.md) (suítes, ações, parâmetros, ABI variádica por plataforma, tabela de plug-ins de teste que passaram).
Os plug-ins aparecem na biblioteca de efeitos ("OpenFX"), com parâmetros animáveis por keyframes.

## O que ficou de fora
GPU (OpenGL/CUDA/Metal), interacts, contextos Transition/Retimer/Paint — ver OFX.md.

## Como rodar
`scripts/build-ofx-examples.sh` e `KOMPO_OFX_TEST_DIR=target/ofx-examples cargo test -p kompo-ofx`; `cargo run -p kompo-ofx --example ofx_report -- <pasta>`.
Plug-ins de terceiros: coloque os bundles em `~/.OFX/Plugins` (Linux), `/Library/OFX/Plugins` (macOS) ou `%CommonProgramFiles%\OFX\Plugins` (Windows).

## Testes
`kompo-ofx/tests/openfx_examples.rs` (6): carregam todos os exemplos do projeto OpenFX, renderizam e conferem pixels. Pulados se a pasta não existir; o CI compila os exemplos.

## Benchmarks
Custo = readback + render CPU + upload; ver PERFORMANCE.md.
