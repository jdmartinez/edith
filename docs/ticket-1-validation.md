# Validación de tres terminales y reconexión

Evaluamos **GPUI 0.2.2** para la interfaz, **vt100 0.15.2** para interpretar el flujo de terminal y **portable-pty 0.9.0** para abrir y redimensionar las PTY. El punto de partida fue el prototipo local `ns-term/prototypes/gpui-terminal` citado en la especificación y el ADR 0005. Los resultados de macOS justifican mantener esta combinación para la siguiente prueba. La elección definitiva queda pendiente de Windows nativo y de GitHub Copilot CLI. [Ticket 1](https://github.com/jdmartinez/edith/issues/1)

## Evidencia del 21 de noviembre de 2026

| Plataforma y flujo | Resultado observado |
| --- | --- |
| macOS ARM64, host con tres shells | El binario de prueba recuperó los PID 69227, 69228 y 69229 entre conexiones. Cada shell ejecutó `stty size` y devolvió 18 filas y 60 columnas junto con su marcador. |
| macOS ARM64, escritorio con tres shells | Los tamaños visibles pasaron de 58×38, 58×17 y 58×17 a 65×38, 50×13 y 50×21 al arrastrar ambos separadores. Al seleccionar y escribir desde la barra lateral, cada shell mostró su propio marcador y tamaño. Al cerrar y reabrir, se recuperaron la disposición, los PID 71867, 71868 y 71869 y la salida del comando que quedó ejecutándose. |
| macOS ARM64, Claude Code 2.1.326 | La flecha abajo movió la selección en la pantalla de inicio de sesión. El panel se redibujó de 58×38 a 65×38 y reapareció con el PID 73305 tras reabrir la ventana. Queda por comprobar una conversación autenticada. |
| macOS ARM64, Codex 0.169.0 | Escape descartó el aviso de actualización y abrió el editor interactivo. El panel se redibujó de 58×17 a 50×20 y reapareció con el PID 73306. |
| macOS ARM64, GitHub Copilot CLI | El ejecutable no estaba disponible en `PATH`. El fallo de arranque quedó en su propio panel y los otros agentes siguieron activos. |
| Windows x64 | Pasó `cargo check --locked --offline --no-default-features --target x86_64-pc-windows-msvc`. La comprobación cruzada de GPUI en el entorno sin red quedó detenida por la dependencia `dwrote 0.11.5`, ausente en la caché. No se dispuso de un escritorio Windows nativo. |

## Reproducción

En macOS, ejecuta `SHELL=/bin/sh cargo run --locked --no-default-features -- --smoke-test`. En Windows nativo, ejecuta el mismo comando de Cargo sin asignar `SHELL`. El smoke test usa Windows PowerShell para consultar las dimensiones de la consola. La fase roja esperaba el marcador con las dimensiones reales y falló porque el shell solo devolvía el marcador. La fase verde consultó las dimensiones dentro de cada PTY y comprobó el marcador completo en su propia línea para descartar el eco del comando.

1. Ejecuta `cargo run --locked`. Selecciona cada sesión desde la barra lateral, ejecuta un comando diferente y comprueba que aparece en el panel correspondiente.
2. Arrastra ambos separadores. Comprueba que las dimensiones coinciden con `stty size` en macOS o con `mode con` en Windows. Deja un comando demorado, cierra la ventana y vuelve a abrirla. Comprueba la salida, la disposición y los mismos PID.
3. Detén el host con `cargo run --locked -- --stop-host` y reinicia con `cargo run --locked -- --agents`. Usa teclas de navegación en cada agente y repite el redimensionado y el cierre. Comprueba el estado visible de los agentes ausentes si alguno no arranca.

## Pendientes

- Ejecutar los flujos de escritorio y agentes en Windows nativo, incluyendo el smoke test con ConPTY.
- Probar GitHub Copilot CLI en ambos sistemas y una conversación autenticada de Claude Code en macOS.
- Revisar el redibujado bajo carga y otras entradas de terminal de los agentes antes de confirmar GPUI y vt100 para la aplicación.
