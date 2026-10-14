# Ondar 0.1.0, versión de prueba

Gracias por probarlo. Ondar es un pequeño reproductor de radio que vive en la barra de menús: eliges un país, eliges una emisora, escuchas. Es un proyecto de Martín Vieira, construido durante unas prácticas en CrabNebula. Esta es la primera versión que sale de mi propio Mac, así que tu tarea es usarlo como una app normal y contarme qué resulta incómodo, qué falla y qué falta.

## Qué necesitas

Un Mac con Apple Silicon (M1 o posterior) con macOS 12 o más reciente. Los Mac con Intel no están soportados en esta versión.

## Instalación

1. Descarga Ondar desde https://cdn.crabnebula.app/download/metambuy/ondar/latest/platform/dmg-aarch64.
2. Abre el DMG y arrastra Ondar a Applications. Hazlo antes del primer arranque; importa para las actualizaciones más adelante.
3. Abre Ondar desde Applications. No hay icono en el Dock ni ventana: busca el icono de Ondar en la barra de menús, arriba a la derecha de la pantalla, y haz clic en él. macOS no debería mostrar ningún aviso de seguridad; si lo muestra, para y dímelo, eso es un fallo mío.

## Qué funciona

Lista de países con búsqueda, lista de emisoras, reproducir y pausar, volumen, favoritos y recientes, el título de la pista en directo cuando la emisora lo envía, y una vista ampliada con un mapa del país seleccionado que muestra dónde están las emisoras. Haz clic en un punto del mapa para filtrar la lista a ese lugar.

## Qué no está todavía (no hace falta que lo reportes)

No hay controles de ecualizador ni visualización de espectro. No hay teclas multimedia ni integración con Now Playing. No hay temporizador de apagado. El icono de la barra de menús no se anima mientras reproduce. Algunas emisiones no se reproducirán: las emisiones HE-AAC v2 y Opus, y algunas emisoras cuya emisión simplemente está caída. Cuando una emisión falla, la app lo dice; si no lo dice, dímelo.

## Qué quiero saber

Cualquier cosa que te haya confundido, cualquier cosa que se haya roto, cualquier cosa que esperabas y no encontraste. Con tres líneas basta: qué hiciste, qué esperabas, qué pasó. Una captura de pantalla ayuda. Envíamelo por el mismo canal por el que te he enviado esto.

## El archivo de registro

Cuando algo vaya mal, adjunta el registro de hoy: en el Finder pulsa Cmd+Shift+G y pega `~/Library/Logs/eu.ondar.radio/`. El archivo se llama `ondar.<fecha>.log`. Contiene nombres de emisoras y direcciones de emisión, y nada más sobre ti.

## Actualizaciones

Cuando haya una versión nueva lista, aparece una línea en la parte de abajo del panel: "Ondar 0.1.1 available · Update". Haz clic en Update; la app se reemplaza a sí misma y se vuelve a abrir. Esto solo funciona si Ondar está en Applications (paso 2).

## Privacidad

Nada sale de tu Mac salvo las emisiones de radio y las peticiones al directorio de emisoras (radio-browser.info). Sin analíticas, sin informes de fallos, sin cuenta.

## Para salir

Haz clic derecho (o Control-clic) en el icono de Ondar en la barra de menús y elige Quit Ondar; el panel en sí no tiene ningún control para salir.

Martín
