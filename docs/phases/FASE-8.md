# Fase 8 — Proxies, cache, otimização e empacotamento

## O que foi implementado
- **Proxies** all-intra H.264 em 1/2, 1/4, 1/8 (`kompo-media/src/proxy.rs`, fila em segundo plano em `kompo-engine/src/proxy.rs`); menu **Media › Use Proxies / Create Proxies**, menu de contexto do media pool (Proxy › 1/2, 1/4, 1/8, Remove) e indicador de progresso no item.
- **Miniaturas** (filmstrip) nos clipes de vídeo da timeline e **formas de onda** em cache de disco; geração em threads de fundo; **Media › Clear Cache** mostra e libera o espaço.
- Reprodução com proxy (`Player::use_proxies`); exportação sempre no original.
- **Empacotamento**: `packaging/build-appimage.sh` (AppImage), `packaging/build-dmg.sh` (.app + .dmg), `packaging/build-msi.ps1` (WiX v4 → .msi), ícone e `.desktop`; workflow `.github/workflows/release.yml` (tags `v*`).
- CI: compila os exemplos OFX e roda seus testes; benchmarks em `bench.yml`.

## O que ficou de fora
- Os pacotes **não foram construídos neste container** (só Linux sem AppImage tools; macOS/Windows só no CI de release) — os scripts estão escritos mas só serão validados na primeira execução do workflow.
- Assinatura/notarização do macOS e assinatura Authenticode do .msi (precisam de certificados).
- Proxies automáticos na importação e transcodificação para ProRes.
- Atualizador automático.

## Como rodar
`Media › Create Proxies for All Video`, depois `Media › Use Proxies`. Pacotes: `packaging/build-appimage.sh`, `packaging/build-dmg.sh`, `pwsh packaging/build-msi.ps1`.

## Testes
`kompo-engine/tests/proxy.rs` (2): proxy 320×180, mesma contagem de quadros, quadro exato; miniaturas geradas em segundo plano. Suíte completa: 146 testes (`cargo test --workspace --no-fail-fast`), todos passando; `cargo clippy --workspace --all-targets -- -D warnings` e `cargo fmt --check` limpos.

## Benchmarks
Resultados no ambiente sem GPU em `docs/PERFORMANCE.md`: decode 4K 46 fps (H.264) / 33 fps (HEVC), abertura de 500 clipes 355 ms, scrub 5,8 ms de UI, memória +0,4%; a meta M1 (4K × 3 camadas × 2 efeitos) **não é atingida sem GPU/decode por hardware** (gargalo documentado).
