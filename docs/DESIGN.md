# Kompo — Design System

Este documento é a fonte única de verdade para a aparência do Kompo. Toda tela
é montada a partir dos tokens abaixo; nenhum valor de cor, espaçamento ou fonte
é escrito "solto" no código da interface. Os tokens vivem em
`crates/kompo-app/src/theme.rs` e espelham exatamente esta página.

## Princípios

1. **A imagem é a protagonista.** A interface é escura e neutra para não
   contaminar a percepção de cor. Nada na tela compete com o viewer.
2. **Uma única cor de destaque.** O âmbar Kompo marca *o que está ativo*:
   seleção, playhead, foco de teclado, valor alterado, botão primário. Se algo
   não está ativo, não é âmbar.
3. **Cada controle justifica a presença.** Um controle que não serve à tarefa
   da página atual não aparece. Painéis secundários se escondem quando não há
   nada para mostrar (inspector sem seleção, media pool durante a edição em
   tela cheia, scopes fora da página Color).
4. **Hierarquia por luminância, não por bordas.** Superfícies se separam por
   pequenos degraus de cinza e, quando inevitável, por uma linha de 1 px de
   baixo contraste. Sem molduras pesadas, sem sombras decorativas, sem
   gradientes.
5. **Sem ícones decorativos.** Rótulos curtos em texto. Glifos só onde são
   universais e funcionais (transporte: play, pause, quadro anterior/seguinte),
   desenhados como vetores simples na cor do texto.
6. **Latência percebida zero.** Toda resposta visual a um gesto acontece no
   mesmo quadro. Animações são curtas (≤ 120 ms) e nunca bloqueiam entrada.

## Cores

Todas as cores são sRGB. Superfícies são cinzas neutros levemente frios
(matiz ~240°, saturação < 6%) para não puxar a percepção de cor da imagem.

### Superfícies

| Token           | Hex       | Uso                                                        |
|-----------------|-----------|------------------------------------------------------------|
| `bg.base`       | `#111113` | Fundo da janela, área atrás do viewer                      |
| `bg.panel`      | `#17171A` | Painéis (timeline, inspector, media pool, mixer)           |
| `bg.raised`     | `#1E1E22` | Cabeçalhos de trilha, campos, régua da timeline            |
| `bg.hover`      | `#26262B` | Hover em linhas, botões e campos                           |
| `bg.active`     | `#2E2E34` | Botão pressionado, item arrastado                          |
| `line`          | `#28282D` | Separador de 1 px (única "borda" permitida)                |
| `line.strong`   | `#3A3A41` | Grade da timeline em zoom alto, contorno de foco sem foco  |

### Texto

| Token            | Hex       | Uso                                         |
|------------------|-----------|---------------------------------------------|
| `text.primary`   | `#E8E8EB` | Valores, nomes de clipe, títulos            |
| `text.secondary` | `#9B9BA3` | Rótulos de parâmetro, timecode da régua     |
| `text.tertiary`  | `#64646C` | Dicas, unidades, estados desabilitados      |
| `text.on-accent` | `#1A1306` | Texto sobre fundo âmbar                     |

### Destaque (a única cor)

| Token           | Hex                    | Uso                                                       |
|-----------------|------------------------|-----------------------------------------------------------|
| `accent`        | `#F2A43A`              | Playhead, seleção, foco, keyframe selecionado, botão primário |
| `accent.hover`  | `#F7B963`              | Hover sobre elementos âmbar                               |
| `accent.dim`    | `#F2A43A` a 22% alfa   | Preenchimento de seleção (clipes, intervalo in/out)       |
| `accent.faint`  | `#F2A43A` a 10% alfa   | Região de snapping, área de drop                          |

### Dados (neutros, não competem com o destaque)

| Token              | Hex       | Uso                                                       |
|--------------------|-----------|-----------------------------------------------------------|
| `clip.video`       | `#34363D` | Corpo de clipe de vídeo                                   |
| `clip.audio`       | `#2B2D32` | Corpo de clipe de áudio                                   |
| `clip.motion`      | `#3A3742` | Clipe de composição Motion                                |
| `clip.label`       | `#C9C9CF` | Nome do clipe                                             |
| `waveform`         | `#8E8E97` | Forma de onda                                             |
| `keyframe`         | `#C9C9CF` | Keyframe não selecionado                                  |

### Semântica (uso restrito a estado, nunca decoração)

| Token     | Hex       | Uso                                                   |
|-----------|-----------|-------------------------------------------------------|
| `danger`  | `#E5484D` | Clip de áudio (0 dBFS), erro de render, mídia offline |
| `ok`      | `#4CC38A` | Somente o "lock" de tracking bem-sucedido             |

Medidores de áudio usam `text.secondary` até −6 dBFS, `accent` de −6 a 0 dBFS
e `danger` acima de 0 dBFS (clip). Scopes de cor usam as cores dos próprios
canais (R, G, B) porque ali a cor *é* o dado.

## Tipografia

- **UI:** Inter (SIL OFL 1.1), embarcada em `assets/fonts`.
- **Numérico/timecode:** JetBrains Mono (SIL OFL 1.1). Dígitos de largura fixa
  evitam que o timecode "tremule" durante a reprodução.

| Token          | Família        | Tamanho | Uso                                       |
|----------------|----------------|---------|-------------------------------------------|
| `type.caption` | Inter          | 11 px   | Régua, unidades, dicas                    |
| `type.body`    | Inter          | 12.5 px | Rótulos, listas, nomes de clipe           |
| `type.strong`  | Inter Medium   | 12.5 px | Título de seção do inspector, página ativa|
| `type.title`   | Inter Medium   | 15 px   | Diálogos, nome do projeto                 |
| `type.mono`    | JetBrains Mono | 12 px   | Valores numéricos, timecode de trilha     |
| `type.tc`      | JetBrains Mono | 18 px   | Timecode principal do viewer              |

Regras: no máximo dois pesos (Regular e Medium). Sem itálico na interface.
Sem caixa alta em textos longos; rótulos de seção usam caixa normal.

## Espaçamento e grade

Base de 4 px. Só estes valores são usados:

| Token    | px |
|----------|----|
| `space.1`| 2  |
| `space.2`| 4  |
| `space.3`| 8  |
| `space.4`| 12 |
| `space.5`| 16 |
| `space.6`| 24 |
| `space.7`| 32 |

- Altura de linha de controle: 22 px. Altura de linha de lista: 24 px.
- Padding interno de painel: 8 px. Distância entre grupos: 16 px.
- Trilhas de vídeo: 48 px (mínimo 28, máximo 120). Trilhas de áudio: 40 px.
- Régua da timeline: 24 px. Barra de páginas: 28 px.

## Raios e linhas

| Token        | Valor | Uso                                 |
|--------------|-------|-------------------------------------|
| `radius.sm`  | 2 px  | Campos, botões, clipes              |
| `radius.md`  | 4 px  | Painéis flutuantes, menus           |
| `stroke`     | 1 px  | Separadores, contorno de seleção    |
| `stroke.focus` | 1.5 px | Contorno âmbar de seleção/foco  |

## Movimento

| Token          | Duração | Curva     | Uso                                  |
|----------------|---------|-----------|--------------------------------------|
| `motion.fast`  | 80 ms   | ease-out  | Hover, pressão                       |
| `motion.panel` | 120 ms  | ease-out  | Painel aparecendo/escondendo         |

A troca de página é **instantânea** (0 ms): o estado de todas as páginas fica
em memória e só o layout muda.

## Componentes

- **Botão:** texto `type.body` em `text.secondary`, sem fundo; hover `bg.hover`;
  ativo/ligado em `accent`. Botão primário (Exportar, Renderizar) é o único com
  fundo `accent`.
- **Campo numérico:** fundo `bg.raised`, texto `type.mono`. Arrastar
  horizontalmente altera o valor (estilo After Effects); duplo clique edita.
  Valor diferente do padrão aparece em `text.primary`, padrão em
  `text.secondary`. Duplo clique no rótulo restaura o padrão.
- **Slider:** trilho de 2 px em `line.strong`, preenchimento em
  `text.tertiary`, alça de 8 px; o valor aparece como campo numérico ao lado.
- **Lista/árvore:** linhas de 24 px, seleção com fundo `accent.dim` e texto
  `text.primary`.
- **Abas de página:** texto apenas (Cut, Color, Audio, Effects, Motion),
  centralizadas na barra inferior; a página ativa fica em `accent` com um traço
  de 2 px acima do rótulo.
- **Painel recolhível:** quando oculto, fica uma faixa de 6 px que revela o
  painel ao passar o mouse ou ao usar o atalho do painel.

## Layout das páginas

Todas compartilham: barra de páginas na base (28 px), viewer centralizado e a
mesma seleção/playhead.

- **Cut:** media pool à esquerda (oculto se vazio), viewer no centro, inspector
  à direita (só com seleção), timeline ocupando a metade inferior.
- **Color:** viewer grande, scopes à direita, faixa de clipes (miniaturas) acima
  das ferramentas; rodas/curvas/qualificador na faixa inferior em abas de texto.
- **Audio:** mixer ocupa a metade inferior (um canal por trilha + master),
  timeline compacta acima, medidores LUFS no master.
- **Effects:** biblioteca à esquerda (busca por texto), viewer no centro, pilha de
  efeitos e editor de keyframes à direita/abaixo.
- **Motion:** lista de camadas à esquerda, viewer da composição no centro,
  propriedades à direita e graph editor na faixa inferior.

## Acessibilidade

- Contraste mínimo de 7:1 para `text.primary` sobre `bg.panel`
  (#E8E8EB / #17171A ≈ 14.6:1) e 4.5:1 para `text.secondary` (≈ 6.4:1).
- Todo comando tem atalho de teclado; foco de teclado visível em âmbar.
- Escala de interface (`zoom_factor`) ajustável de 75% a 200%.
