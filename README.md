# Edith

Edith abre sesiones de Codex para repositorios locales y las mantiene activas al cerrar la ventana.

```sh
cargo run --locked
```

1. Pulsa **Añadir repositorio** y selecciona una carpeta dentro del repositorio. Para añadir varios proyectos, pulsa **Escanear carpeta…**, elige la carpeta contenedora, marca los clones que quieras ver y pulsa **Añadir seleccionados**. Cada clon local tiene su propio proyecto y muestra sus worktrees debajo.
2. Pulsa **Iniciar Codex** en el proyecto. Edith detecta Codex en `PATH`. Usa **Elegir ejecutable** para dar prioridad a otra instalación. Codex hereda la configuración y el entorno del usuario.
3. Selecciona una sesión desde la barra lateral o su pestaña para enviarle el teclado. El control en la cabecera permite ocultar y volver a mostrar la barra lateral.
4. Cierra y vuelve a abrir la ventana para recuperar las sesiones, incluidos el estado y la última pantalla disponible de las que terminaron.

`cargo run --locked -- --status` consulta el host y `cargo run --locked -- --stop-host` lo detiene junto con sus sesiones. Los proyectos y la ruta elegida se conservan mientras viva el host. Puedes indicar un directorio de estado privado y aislado con `EDITH_PROTOTYPE_STATE`.

El prototipo del ticket 1 sigue accesible con `--prototype` para tres shells y con `--agents` para Claude Code, Codex y Copilot CLI. Usa un directorio de estado distinto al del escritorio. Consulta la [validación del prototipo](docs/ticket-1-validation.md).
