# Edith

Edith abre sesiones de Claude Code, Codex y GitHub Copilot CLI para repositorios locales y las mantiene activas al cerrar la ventana.

```sh
cargo run --locked
```

1. Pulsa **Añadir repositorio** y selecciona una carpeta dentro del repositorio. Para añadir varios proyectos, pulsa **Escanear carpeta…**, elige la carpeta contenedora, marca los clones que quieras ver y pulsa **Añadir seleccionados**. Cada clon local tiene su propio proyecto y muestra sus worktrees debajo.
2. Pulsa **Nueva sesión** en el proyecto y elige un agente. Edith busca el ejecutable en `PATH` y en ubicaciones habituales de instalación. En **Ejecutables de agentes**, usa **Elegir ejecutable** junto al agente correspondiente para dar prioridad a otra instalación, o **Usar detección automática** para restablecer la búsqueda. Cada agente hereda el entorno del usuario y usa su configuración y autenticación habituales.
3. Selecciona una sesión desde la barra lateral o su pestaña para enviarle el teclado. El control en la cabecera permite ocultar y volver a mostrar la barra lateral.
4. En el menú **Panel**, elige **Dividir a la derecha** o **Dividir debajo**. Abre una sesión o selecciona una oculta para llenar el espacio libre. Arrastra el separador para ajustar los tamaños.
5. En el mismo menú, **Ocultar sesión** retira el panel y mantiene el agente activo. La barra lateral marca la sesión como **Oculta** y permite restaurarla. **Cerrar sesión** retira el panel y detiene el agente.
6. Cierra y vuelve a abrir la ventana para recuperar la disposición, los tamaños y las sesiones desde el host.

`cargo run --locked -- --status` consulta el host y `cargo run --locked -- --stop-host` lo detiene junto con sus sesiones. Los proyectos y las rutas elegidas se conservan mientras viva el host. Puedes indicar un directorio de estado privado y aislado con `EDITH_PROTOTYPE_STATE`.

El prototipo del ticket 1 sigue accesible con `--prototype` para tres shells y con `--agents` para Claude Code, Codex y Copilot CLI. Usa un directorio de estado distinto al del escritorio. Consulta la [validación del prototipo](docs/ticket-1-validation.md).

## Estadísticas locales de Copilot

Edith busca registros `session.usage_record` en `~/.copilot/session-state/<sesión>/events.jsonl` o en la ubicación definida por `COPILOT_HOME`. Guarda los contadores de tokens y el tiempo de API que aparecen en cada registro. En el panel, `0` indica un cero registrado, `Sin datos` indica un contador ausente y `parcial` identifica un total calculado con registros incompletos. [Directorio de configuración de Copilot CLI](https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-config-dir-reference)

El formato se comprobó en registros nativos de `copilot-agent` en macOS. La cobertura de instalaciones independientes de Copilot CLI en macOS y Windows está pendiente de verificación.
