# Fase 2 — Exportação

## O que foi implementado

- **Motor de exportação** (`kompo-engine/src/export.rs`), com três estágios em
  paralelo: decoders adiantados (clipes que vão começar são abertos antes),
  render na GPU com a transformação de saída **e o empacotamento nos planos do
  encoder** feitos num compute shader (`kompo-gpu/src/shaders/pack.wgsl`, com
  paridade bit a bit com a versão CPU), e uma thread de encode/mux.
- **Seleção de encoder**: hardware primeiro (NVENC, Quick Sync, AMF,
  VideoToolbox, Media Foundation), testando se abre de verdade na máquina;
  fallback para software (libx264, libx265, libvpx-vp9, SVT-AV1/rav1e/aom,
  prores_ks, dnxhd).
- **Formatos**: H.264, HEVC (8/10 bits, tag `hvc1`), ProRes 422 Proxy/LT/422/HQ/
  4444/4444 XQ (com alfa), DNxHR LB/SQ/HQ/HQX/444, VP9 (WebM), AV1; MP4, MOV,
  MKV, WebM; sequências PNG 8/16, JPEG, OpenEXR (half, linear); áudio WAV
  16/24, AAC, FLAC, Opus. Cor de entrega Rec.709, HDR10 (PQ) e HLG.
- **Presets** (19), qualidade constante (CRF/CQ) ou bitrate, meia resolução,
  faixa sequência inteira ou In/Out.
- **Fila de render** em segundo plano (um job por vez, cancelável, progresso,
  fps e tempo restante), usando o mesmo device da GPU da interface.
- **Interface**: File › Export Media… (**Ctrl+M**), Export Frame…
  (**Ctrl+Shift+E**, PNG/JPEG/EXR) e janela Render Queue.
- Importação de sequências de imagens numeradas.

## O que ficou de fora

- Encoders VAAPI (exigem upload para superfícies de hardware) — no Linux o
  encode por hardware fica para a Fase 8.
- Metadados HDR10 de masterização só no libx265 (valores fixos P3-D65 1000 nits).
- Presets personalizados salvos pelo usuário.

## Como rodar

`cargo run --release` → montar a timeline → **Ctrl+M** → escolher preset →
*Add to Queue*.

## Testes

`cargo test --workspace` — **92 testes, todos passando**. Novos:
`kompo-engine/tests/export.rs` (7): cada formato acima é exportado, decodificado
de volta e conferido **quadro a quadro** (número de quadro codificado na
imagem), tamanho e contagem; sequências reimportadas; áudio com duração
correta; fila com conclusão e cancelamento. Paridade GPU/CPU do empacotamento
em `kompo-gpu/tests/parity.rs`.

## Benchmarks

Neste container (sem GPU, llvmpipe): a suíte de exportação inteira (17
arquivos de 24 quadros 640×360) roda em ~5 s. O gargalo em 4K é o mesmo da
Fase 0 (rasterização por software), documentado em `docs/PERFORMANCE.md`.
