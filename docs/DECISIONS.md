# Kompo — Registro de decisões

Cada decisão tem contexto, escolha, alternativas consideradas e consequências.
Decisões novas são acrescentadas ao final; decisões revistas são marcadas como
"substituída por" em vez de apagadas.

---

## D1. Linguagem e estrutura do workspace

**Escolha.** Todo o código do aplicativo é Rust (edição 2024), organizado em um
workspace Cargo com as crates pedidas:

| Crate          | Responsabilidade                                                          |
|----------------|---------------------------------------------------------------------------|
| `kompo-core`   | Modelo de timeline, tempo, keyframes, undo/redo, arquivo de projeto, EDL/FCPXML |
| `kompo-media`  | Decode/encode (FFmpeg), imagens (PNG/JPEG/EXR), proxies, cache            |
| `kompo-gpu`    | Grafo de render: backend wgpu/WGSL e backend CPU (rayon)                  |
| `kompo-audio`  | Conform (symphonia + rubato), mixer, DSP, medidores, saída cpal           |
| `kompo-color`  | Transferências, primárias, Y'CbCr, LUTs, leitor de config OCIO, correção  |
| `kompo-motion` | Composições em camadas, texto, formas, parenting, expressões              |
| `kompo-ofx`    | Host OpenFX via FFI                                                        |
| `kompo-engine` | **(acrescentada)** orquestração headless: reprodução, render de timeline, fila de exportação |
| `kompo-app`    | Interface (egui)                                                           |

**Por que `kompo-engine`.** A exportação e a reprodução precisam combinar core,
mídia, GPU e áudio. Esse código não pode morar no core (que não depende de
nada), nem em `kompo-media` (que não deve depender da GPU), e colocá-lo na
interface impediria rodar exportação, benchmarks e testes sem janela. A crate
extra mantém a regra "o core não depende da interface" e deixa todo o pipeline
testável headless.

**Grafo de dependências.** `core` ← `color` ← `gpu` (+ `media`) ;
`core` ← `audio` ; `core` ← `motion` ; `ofx` independente ;
`engine` depende de todas ; `app` depende de `engine`. Nenhuma crate abaixo de
`app` conhece egui/winit.

---

## D2. Interface: egui (via eframe com backend wgpu)

**Alternativas avaliadas:** egui, iced, Slint.

| Critério                                   | egui | iced | Slint |
|--------------------------------------------|------|------|-------|
| Latência de entrada                        | entrada processada no mesmo quadro em que é desenhada (modo imediato) | atualização via mensagens + diff da árvore de widgets | DSL compilada, propriedades reativas; bom, mas um passo a mais |
| Widgets densos e customizados (timeline com milhares de clipes, scopes, curvas, graph editor) | `Painter` direto, culling trivial, sem árvore retida | `Canvas` com cache de geometria; custom widgets exigem mais cerimônia | exige render para imagem ou underlay; pouco natural para milhares de itens |
| Integração com wgpu (viewer sem cópia CPU) | `egui-wgpu` expõe o mesmo `Device`/`Queue`; texturas nativas registradas como `TextureId` | possível via widget `shader`, API menos estável | possível via underlay/`BorrowedTexture`, mais acoplado |
| Maturidade/ecossistema para ferramentas    | amplamente usado em ferramentas (Rerun, editores) | crescendo | forte em UI embarcada/formulários |
| Licença                                    | MIT/Apache-2.0 | MIT | GPLv3 / royalty-free com atribuição / comercial |

**Escolha: egui 0.36 + eframe com wgpu 30.**

Motivos principais: (1) latência: o gesto do usuário (arrastar um clipe,
mover o playhead) altera o estado e é desenhado no mesmo quadro, sem fila de
mensagens nem reconciliação; (2) a timeline, os scopes, as rodas de cor e o
graph editor são widgets desenhados à mão, o caso ideal do modo imediato;
(3) o viewer renderiza direto numa textura do mesmo device que a UI usa — zero
cópia entre GPU e CPU; (4) licença permissiva.

**Riscos e mitigação.** Modo imediato redesenha a UI inteira a cada quadro.
Mitigações: culling de clipes fora da área visível, formas de onda e
miniaturas pré-computadas em níveis (mipmaps de picos), galleys de texto
cacheados pelo próprio egui, e repaint apenas quando há mudança (o app fica
parado sem consumir CPU quando nada acontece). O benchmark
`ui_timeline_heavy` mede o tempo de CPU por quadro com timelines grandes (ver
`docs/PERFORMANCE.md`).

---

## D3. Decodificação e exportação: FFmpeg via `ffmpeg-next` + `ffmpeg-sys-next`

**Escolha.** Bindings `ffmpeg-next` 9 (API segura) e `ffmpeg-sys-next` 9 (FFI
bruta, usada para aceleração por hardware, seek preciso e detalhes de
colorspace do swscale), ligando **dinamicamente** às bibliotecas `libavcodec`,
`libavformat`, `libavutil`, `libswscale` e `libswresample` (FFmpeg ≥ 6.1).

**Alternativas.** `rsmpeg` (bindings mais finos, menos cobertura segura);
GStreamer (`gstreamer-rs`, pipeline pesado para acesso quadro-a-quadro);
decoders puros em Rust (cobrem poucos codecs de vídeo profissionais).
FFmpeg é o único que cobre H.264, HEVC, ProRes, DNxHR, VP9, AV1 e os
aceleradores de hardware num só lugar.

**Aceleração por hardware.** Decodificação tenta, nesta ordem e por
plataforma: Linux — NVDEC (CUDA), VAAPI; Windows — NVDEC, D3D11VA, DXVA2;
macOS — VideoToolbox. Falha de inicialização cai para software multi-thread
sem intervenção do usuário, e o tipo que falhou não é tentado de novo no mesmo
processo. O quadro é transferido para memória de sistema
(`av_hwframe_transfer_data`, formatos NV12/P010) e enviado à GPU de render;
interop zero-cópia (DMA-BUF/Vulkan, D3D11 shared, IOSurface/Metal) fica como
otimização futura (ver `docs/PERFORMANCE.md`).

### Implicação de licença (LGPL × GPL)

- O FFmpeg em si é **LGPL 2.1+**. Componentes opcionais o tornam **GPL**
  (`--enable-gpl`: libx264, libx265, libxvid, frei0r, vidstab...) ou
  **não redistribuível** (`--enable-nonfree`: fdk-aac, NDI...).
- O Kompo **liga dinamicamente** ao FFmpeg. Com um build **LGPL**, o Kompo
  pode ser distribuído sob qualquer licença, desde que (a) as bibliotecas do
  FFmpeg sejam distribuídas como bibliotecas compartilhadas substituíveis pelo
  usuário, (b) o código-fonte do FFmpeg usado (e a configuração) esteja
  disponível e (c) a licença LGPL seja incluída. O empacotamento (Fase 8) faz
  exatamente isso.
- **Se** for distribuído um build **GPL** (com libx264/libx265), o binário
  combinado passa a estar sujeito à **GPL v2+** e o Kompo inteiro teria de ser
  distribuído como GPL. Por isso os pacotes oficiais usam um FFmpeg **LGPL**.
- Consequência prática para os codecs de exportação no build LGPL:
  - H.264/HEVC: encoders de hardware (NVENC, Quick Sync, AMF, VAAPI,
    VideoToolbox, MediaFoundation) — todos compatíveis com LGPL; em software,
    **OpenH264** (BSD, binário da Cisco) para H.264 e **Kvazaar**/**SVT-HEVC**
    (BSD) quando presentes. Se o usuário instalar um FFmpeg GPL no sistema, o
    Kompo detecta libx264/libx265 em tempo de execução e os oferece — o
    usuário usa, mas o Kompo não os redistribui.
  - ProRes (`prores_ks`), DNxHR (`dnxhd`), FFV1, PCM, FLAC, AAC nativo: no
    próprio FFmpeg LGPL.
  - VP9 (`libvpx`, BSD) e AV1 (`libsvtav1`/`libaom`, BSD; `librav1e`, BSD):
    compatíveis com LGPL.
- O app mostra em *Sobre* a licença do FFmpeg carregado
  (`kompo_media::ffmpeg_version()`), e o pacote inclui `LICENSES/FFmpeg-LGPL`.
- Patentes (H.264/HEVC/AAC) são uma questão separada de copyright e dependem
  do território e do modelo de distribuição; documentado aqui para a decisão
  comercial, fora do escopo do código.
- O ambiente de desenvolvimento usa o FFmpeg do Ubuntu (build GPL). Isso é
  aceitável para desenvolvimento e CI, **não** para distribuição.

---

## D4. Áudio: cpal, symphonia, rubato (+ ebur128)

- **cpal 0.18** para saída (WASAPI, CoreAudio, ALSA/PulseAudio/PipeWire).
  Sem dispositivo (CI, servidores), um "dispositivo nulo" consome amostras em
  tempo real para que o relógio continue funcionando.
- **symphonia 0.6** para decode (WAV, FLAC, AAC/ALAC em MP4/MOV, MKV, Ogg,
  MP3). Para o que o symphonia não decodifica (Opus, AC-3/E-AC-3, DTS, PCM em
  MXF), o engine cai para o decoder do FFmpeg — o fallback mora em
  `kompo-engine` para que `kompo-audio` não dependa de FFmpeg.
- **rubato 5** (reamostrador FFT síncrono) para converter para a taxa do
  projeto, com compensação do atraso do filtro (a amostra 0 continua
  alinhada à 0).
- **Conform em disco.** O áudio de cada mídia é decodificado uma vez para
  f32 na taxa do projeto e guardado em cache (`<cache>/kompo/audio`), junto com
  um arquivo de picos para as formas de onda. Reprodução e exportação leem com
  leituras posicionais: memória constante em mídias longas e acesso aleatório
  instantâneo (scrub).
- **Tempo real.** O callback do dispositivo só lê de um ring buffer SPSC
  sem lock (`rtrb`); a mixagem roda numa thread própria. Seek usa um
  contador de época para descartar amostras antigas sem travar.
- **Relógio mestre.** A posição audível (amostras consumidas + interpolação
  desde o último callback − latência informada pelo dispositivo) é o relógio
  da reprodução a 1x. O vídeo segue o áudio.
- **LUFS/true peak:** `ebur128` (port em Rust puro do libebur128, MIT).

---

## D5. Representação do tempo: ticks de 1/705 600 000 s

Inteiros de 64 bits em *flicks*. Essa base divide exatamente todas as taxas de
quadro comuns (incluindo 23,976/29,97/59,94) e todas as taxas de amostragem
(44,1/48/96/192 kHz), então limites de quadro e de amostra são inteiros
exatos: sem deriva de ponto flutuante em edições longas. Alcance de ±414 anos.
Conversões usam aritmética em `i128`.

---

## D6. Undo/redo ilimitado com snapshots persistentes

Cada edição gera um novo `Project`; o anterior vai para a pilha de undo.
`Project` usa coleções persistentes (`imbl::Vector`, `imbl::OrdMap`), então
snapshots consecutivos compartilham quase toda a memória — uma edição que
toca um clipe custa aproximadamente um clipe mais alguns nós de árvore.
Gestos contínuos (arrastar slider, mover clipe) são agrupados num único passo.

Alternativa descartada: padrão Command com operações inversas — mais
econômico em memória, mas cada operação precisa de um inverso correto, e um
erro no inverso corrompe o projeto silenciosamente. Snapshots são corretos
por construção.

---

## D7. Formato de projeto `.kompo`

JSON UTF-8 com envelope `{format, version, saved_by, project}`. Migrações
encadeadas (`migrate`) atualizam documentos antigos passo a passo antes da
desserialização; arquivos de versão mais nova são recusados com erro claro.
Salvamento atômico (arquivo temporário + rename) com backup `.kompo.bak`.
Os metadados das mídias ficam no projeto, então abrir um projeto não precisa
reabrir cada arquivo de mídia (essencial para a meta de 500 clipes < 2 s).

---

## D8. Pipeline de cor em ponto flutuante linear

- Todo o processamento ocorre em luz **linear** com alfa **pré-multiplicado**,
  em `f16` na GPU (`Rgba16Float`) e `f32` na CPU.
- Espaço de trabalho configurável por projeto: Rec.709 linear (padrão),
  ACEScg ou Rec.2020 linear.
- Entrada: Y'CbCr → R'G'B' (matriz BT.601/709/2020, faixa limitada ou
  completa, 8–16 bits) → decodificação da transferência → conversão de
  primárias (Bradford) para o espaço de trabalho.
- Vídeo Rec.709 é tratado como referido à tela (EOTF BT.1886, potência 2,4),
  de modo que o caminho entrada → saída Rec.709 é identidade: o que se vê no
  Kompo bate com outros players e com o arquivo exportado.
- HDR: PQ e HLG normalizados pela ITU-R BT.2408 (branco de referência
  203 nits = 1,0; HLG 75% = 1,0).
- Curvas de câmera incluídas: ACEScct, ACEScc, ARRI LogC3, Sony S-Log3,
  Panasonic V-Log.
- As mesmas fórmulas existem em Rust (`kompo-color`) e em WGSL
  (`transfer.wgsl`); os testes comparam GPU e CPU.

**OpenColorIO.** O OCIO é C++ e não tem API C. Para manter todo o código em
Rust, o Kompo inclui um **leitor de configs OCIO v2 em Rust**
(`kompo-color::ocio`, Fase 4) que interpreta o subconjunto usado na prática
(ColorSpaces, Roles, Displays/Views, Looks; transforms Matrix, Exponent,
ExponentWithLinear, Log, LogAffine, LogCamera, CDL, Range, FileTransform
`.cube/.spi1d/.spi3d`, Group, ColorSpaceTransform e os Builtins ACES mais
comuns). Na GPU, o processador resultante é "assado" em LUT 3D 65³ com
shaper logarítmico, como o caminho GPU legado do próprio OCIO. Limitações
documentadas na Fase 4.

---

## D9. Grafo de render com dois backends

`kompo-gpu` descreve cada quadro como um DAG de nós (fonte, imagem, sólido,
composição, efeito, transição, cor) independente de backend. O backend wgpu
executa com shaders WGSL; o backend CPU (rayon) implementa as mesmas fórmulas
e serve de (a) fallback quando não há GPU adequada (`KOMPO_CPU_RENDER=1`
força) e (b) referência para os testes de paridade GPU × CPU. O renderer
mantém pool de texturas reaproveitadas e cache dos últimos quadros
convertidos; texturas ociosas são liberadas (memória estável).

---

## D10. Imagens estáticas: `image` e `exr` (Rust puro)

PNG e JPEG via `image`; OpenEXR via `exr` (half/float, leitura de camadas
RGBA). Escolhidos no lugar do FFmpeg para stills porque preservam
float/16 bits sem conversões intermediárias e não exigem C.

---

## D11. Fontes da interface

Inter (UI) e JetBrains Mono (timecode e valores numéricos), ambas SIL OFL
1.1, embarcadas em `assets/fonts` com as licenças ao lado.

---

## D12. Licença do próprio Kompo

Não foi definida pelo dono do projeto. O desenho (FFmpeg LGPL ligado
dinamicamente, dependências Rust MIT/Apache/BSD/MPL-2.0, fontes OFL) mantém
abertas tanto licenças permissivas quanto proprietárias. `symphonia` é
MPL-2.0 (copyleft por arquivo: modificações nos arquivos do symphonia
precisariam ser publicadas; usar a biblioteca não impõe nada ao Kompo).
