# Modularidad, visibilidad y validación en Rust para Edith

Consulta de documentación oficial del 23 de noviembre de 2026. Edith usa la edición 2024 y activa la feature `ui` por defecto. El punto de partida son [Cargo.toml](../../Cargo.toml), [src/main.rs](../../src/main.rs), [src/host.rs](../../src/host.rs) y [src/ui.rs](../../src/ui.rs).

## Módulos y visibilidad

- Una declaración `mod terminal;` en `src/host.rs` incorpora `src/host/terminal.rs` al árbol como `crate::host::terminal`. Se puede conservar `host.rs` como archivo del módulo padre y usar el mismo esquema para `ui.rs`. La convención con `mod.rs` también es válida. Si un módulo ya existe, trasladar su cuerpo a un archivo conserva su ruta. Extraer un submódulo nuevo sí cambia la ruta de los elementos movidos. [Rust Book, módulos en archivos separados](https://doc.rust-lang.org/book/ch07-05-separating-modules-into-different-files.html) · [Rust Reference, módulos](https://doc.rust-lang.org/reference/items/modules.html)
- Los elementos privados son accesibles desde su módulo y sus descendientes. Para un elemento extraído que usan el padre u otros descendientes del padre, `pub(super)` limita la visibilidad a ese ámbito. Para una interfaz usada desde otras partes del crate, `pub(crate)` expresa ese alcance. `pub(in crate::host)` permite delimitarlo a un antecesor concreto desde un módulo más profundo. Si se mueve un tipo compartido, una reexportación selectiva con `pub(crate) use` en el padre permite conservar rutas como `crate::host::Request`, siempre que el tipo tenga visibilidad suficiente para reexportarlo. [Rust Reference, visibilidad y privacidad](https://doc.rust-lang.org/reference/visibility-and-privacy.html)
- Una prueba unitaria en un submódulo `#[cfg(test)]` puede acceder a elementos privados del módulo que la contiene. Para una regresión observable desde el binario, las pruebas de integración pueden ejercer el punto de entrada existente. Esto permite comprobar el comportamiento sin ampliar la visibilidad para las pruebas. [Rust Book, organización de pruebas](https://doc.rust-lang.org/book/ch11-03-test-organization.html) · [Cargo Book, objetivos de prueba](https://doc.rust-lang.org/cargo/commands/cargo-test.html#target-selection)

## Propuesta para Edith

- **Host.** Empezar por el bloque de operaciones de repositorios y worktrees o por el ciclo de vida de `Terminal`, su PTY y la captura de pantalla. Cada bloque tiene colaboradores identificables. Mantener inicialmente `Endpoint`, `Request`, `Reply` y el despacho en `host.rs` reduce el número de rutas que cambian a la vez. Mover el código junto con sus atributos de plataforma y conservar el formato de los mensajes IPC, incluidos el encuadre y los límites de lectura. [Código del host](../../src/host.rs)
- **UI.** Mantener `ui::run` y la propiedad de `Workspace` en `ui.rs` al comienzo. El transporte de peticiones en `client` y el selector de sesiones ofrecen límites concretos para extraer. Un submódulo puede alojar un bloque `impl Workspace` y acceder al estado privado del padre. Solo los métodos invocados desde fuera de ese submódulo necesitan ampliar su visibilidad. Mantener los submódulos bajo el `#[cfg(feature = "ui")]` existente conserva la compilación sin GPUI al desactivar las features predeterminadas. [Código de la UI](../../src/ui.rs) · [Punto de entrada](../../src/main.rs) · [Cargo Book, features](https://doc.rust-lang.org/cargo/reference/features.html)

## Validación de cada extracción

Registrar el resultado de partida y repetir las comprobaciones con la misma versión de las herramientas después de cada extracción. La ejecución predeterminada incluye la UI. La segunda configuración comprueba que el host y sus pruebas siguen compilando y ejecutándose sin esa feature.

```sh
cargo fmt --all -- --check
cargo check --locked --all-targets
cargo check --locked --all-targets --no-default-features
cargo clippy --locked --all-targets
cargo clippy --locked --all-targets --no-default-features
cargo test --locked
cargo test --locked --no-default-features
```

`cargo fmt` obtiene la edición del manifiesto y `--check` comprueba el formato sin reescribirlo. `cargo check` aporta una comprobación rápida sin generar código final. Clippy analiza los targets seleccionados y conviene comparar sus diagnósticos con los de partida. `cargo test` compila y ejecuta las pruebas. `--locked` exige que Cargo utilice el archivo de bloqueo existente sin actualizarlo. [rustfmt](https://github.com/rust-lang/rustfmt#checking-style-on-a-ci-server) · [Cargo Book, check](https://doc.rust-lang.org/cargo/commands/cargo-check.html) · [Clippy, uso](https://doc.rust-lang.org/clippy/usage.html) · [Cargo Book, test](https://doc.rust-lang.org/cargo/commands/cargo-test.html)

Las pruebas de [desktop_host.rs](../../tests/desktop_host.rs) ejercen el binario por IPC en Unix, incluidos reconexión, worktrees y ciclo de vida de paneles. Para una extracción de UI, complementar la compilación con una prueba manual de arranque, selección de sesión, foco y redimensionado. Si se mueve código de IPC condicionado a Windows, comprobar esa rama en Windows, dado que la suite de Unix no la ejecuta. La nota documenta el procedimiento propuesto y la revisión estática del código.
