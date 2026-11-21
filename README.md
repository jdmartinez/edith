# Edith

Prueba técnica de escritorio en Rust con GPUI, vt100 y portable-pty. Abre tres terminales con dos separadores ajustables. El host conserva las PTY al cerrar la ventana.

```sh
cargo run --locked
cargo run --locked -- --agents
cargo run --locked -- --status
cargo run --locked -- --stop-host
```

El modo `--agents` usa `claude`, `codex` y `copilot` de `PATH`. Para cambiar de modo, detén el host anterior. También puedes usar un directorio privado diferente mediante `EDITH_PROTOTYPE_STATE`.

Consulta la [evidencia y las pruebas pendientes del ticket 1](docs/ticket-1-validation.md).
