# Kompo — Metas de desempenho

As metas são medidas pelo binário `kompo-bench` (crate `kompo-engine`), que
gera a própria mídia de teste (4K com textura em movimento, comprimida como
material real) e roda em CI no workflow **Benchmarks**
(`.github/workflows/bench.yml`). O resultado vai para o resumo do job e para o
artefato `bench-results` (`results.json` + `results.md`).

```sh
cargo run --release -p kompo-engine --bin kompo-bench -- --quick      # ~1 min
cargo run --release -p kompo-engine --bin kompo-bench                 # completo
cargo run --release -p kompo-engine --bin kompo-bench -- --only scrub,memory
```

## Metas

| # | Meta | Cenário no `kompo-bench` | Critério | Depende de hardware? |
|---|------|--------------------------|----------|----------------------|
| M1 | Reprodução em tempo real de 4K H.264 e HEVC com 3 camadas e 2 efeitos, em GPU integrada atual | `playback_4k_h264_3layers_2fx`, `playback_4k_hevc_3layers_2fx` | ≥ 99% dos quadros exibidos no tempo certo, na taxa da fonte (30 fps), tocando contra o relógio de áudio | sim (iGPU + decode por hardware) |
| M1a | Decodificação 4K sustentável | `decode_4k_h264`, `decode_4k_hevc` | ≥ 30 fps por stream | sim |
| M2 | Scrubbing na timeline sem travar | `scrub_4k_h264` | custo da thread de UI < 16,7 ms por quadro (60 fps) em qualquer salto; o quadro exato chega depois, sem bloquear | não |
| M3 | Abrir projeto com 500 clipes em menos de 2 s | `project_open_500` (Fase 1) | carregar + migrar + desserializar + montar o primeiro quadro < 2 s | não |
| M4 | Memória estável em sessões de várias horas | `memory_soak` | crescimento de RSS < 5% entre o 2º e o 4º quartil de uma sessão de render contínuo com saltos (o modo completo roda 120 s; CI 20 s) | não |
| M5 | UI a 60 fps com timeline pesada | `ui_timeline_heavy` (Fase 1) | tempo de CPU para montar e tessellar um quadro da UI com 5 000 clipes < 8 ms | não |

As metas "dependem de hardware" quando o runner do CI não tem o hardware da
meta (os runners do GitHub não têm GPU nem decodificador de vídeo; a GPU vista
pelos testes é o rasterizador em software do Mesa, *lavapipe/llvmpipe*). Nesses
casos o CI reporta o número medido como documentação do gargalo, sem falhar o
job; as metas que não dependem de hardware falham o job (`--strict`).

## Arquitetura que sustenta as metas

- **Decodificação fora da thread de UI.** Um decoder por clipe ativo
  (`kompo-engine/src/frames.rs`), com pedido "o mais recente vence": no
  scrubbing, pedidos intermediários são descartados; na reprodução, o worker
  decodifica adiante numa fila limitada por memória (~96 MiB por decoder) e os
  clipes que começam no próximo segundo são pré-aquecidos.
- **Decodificação por hardware** (NVDEC, VAAPI, D3D11VA, VideoToolbox) com
  fallback automático.
- **Conversão de cor na GPU**: os planos Y'CbCr são enviados como estão
  (metade dos bytes de RGBA8 em 4:2:0) e convertidos num único passe, que já
  reduz para a resolução de exibição com filtro *box* exato usando o bilinear
  do hardware (uma amostra por bloco 2×2).
- **Viewer sem cópia**: o quadro é renderizado direto numa textura do mesmo
  device que o egui usa.
- **Relógio de áudio**: o vídeo segue a posição audível; não há acúmulo de
  deriva.
- **Pools e caches limitados**: texturas reaproveitadas e liberadas após 240
  quadros sem uso; decoders ociosos fecham após 8 s; cache de quadros
  convertidos com LRU.

## Medições

### Fase 0 — container de desenvolvimento (sem GPU)

Ambiente: 4 vCPUs x86-64, sem GPU (Vulkan via **llvmpipe**, rasterização em
software), sem decodificador de hardware, FFmpeg 6.1. Comando:
`kompo-bench --quick`.

| Cenário | Resultado | Meta | Status |
|---|---|---|---|
| decode_4k_h264 | 49,9 fps (software, 1 stream) | ≥ 30 fps | ok |
| decode_4k_hevc | 33,1 fps (software, 1 stream) | ≥ 30 fps | ok |
| playback_4k_h264_3layers | 0 fps no tempo (p95 composição+render 817 ms) | 30 fps, ≥ 99% | **não atingida** (hardware) |
| playback_4k_hevc_3layers | 0 fps no tempo (p95 723 ms) | 30 fps, ≥ 99% | **não atingida** (hardware) |
| scrub_4k_h264 | 5,5 ms de custo máximo na UI; quadro exato p50 340 ms / p95 704 ms | < 16,7 ms | ok |
| memory_soak (20 s) | +0,68% RSS (1301 → 1310 MiB) | < 5% | ok |

**Gargalo documentado (M1 neste ambiente).** Dois fatores, ambos do ambiente
de medição, não da arquitetura:

1. *Rasterização em software.* No llvmpipe, converter **um** quadro 4K para
   1080p custa 52,8 ms (4K→4K: 207 ms); o upload de um plano 4K custa só
   2,5 ms (`cargo run --release -p kompo-gpu --example profile_frame`). Três
   camadas já passam de 160 ms antes da composição — numa GPU integrada atual
   o mesmo passe é de 1–3 ms por camada (o shader faz 3 amostras bilineares por
   pixel de saída).
2. *Decodificação em software com 4 núcleos.* Três streams 4K simultâneos
   precisam de 90 fps de decodificação; a máquina faz ~50 fps de H.264 4K num
   único stream usando todos os núcleos — e esses mesmos núcleos estão
   ocupados com o llvmpipe. Em hardware-alvo, NVDEC/Quick Sync/VideoToolbox
   decodificam vários streams 4K60 em blocos dedicados.

Mitigações já disponíveis para máquinas fracas: resolução de reprodução ½ e ¼
no viewer, o backend CPU (`KOMPO_CPU_RENDER=1`) e — Fase 8 — proxies.
A validação de M1 em hardware real exige rodar `kompo-bench` numa máquina com
GPU integrada (procedimento em "Como medir em hardware real").

### Como medir em hardware real

```sh
cargo build --release -p kompo-engine --bin kompo-bench
./target/release/kompo-bench --out bench-out      # gera 4K H.264/HEVC e mede tudo
cat bench-out/results.md
```

A primeira linha do relatório mostra o adaptador (`GPU: Intel(R) Graphics ...`)
e se a decodificação foi por hardware (coluna *Notes*).
