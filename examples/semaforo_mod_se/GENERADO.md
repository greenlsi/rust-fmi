# Del modelo DEVS al código FMI — seguimiento de lo que genera `#[atomic2fmu]`

Este es el **paso intermedio**: el "código FMI" que la macro produce a partir del modelo,
*antes* de `cargo fmi bundle`. No vive en un `.rs` del fuente (se genera al compilar),
pero la macro lo **vuelca automáticamente a un archivo en cada compilación** para que
puedas seguirlo.

## Dónde verlo (automático, en cada build)

Cada vez que compilas, `#[atomic2fmu]` escribe el código que genera en:

```
target/atomic2fmu/<NombreDeLaFmu>.rs
```

Para este ejemplo: [`target/atomic2fmu/SemaphoreFmu.rs`](target/atomic2fmu/SemaphoreFmu.rs)
— formateado y legible, con las variables que la macro dedujo del modelo (`ta`,
`input_ev`, `input`, `green`, `red`), el `Default`, la `impl UserModel` y el `export_fmu!`.

Es el código exacto (no una reconstrucción). Basta con abrir ese archivo tras `cargo build`.

> Para desactivar el volcado: variable de entorno `ATOMIC2FMU_NO_DUMP=1`.
>
> El archivo vive en `target/` (se regenera y no se versiona). Si quieres guardar una
> instantánea para el informe, cópialo a otro sitio.

## Las tres capas de la expansión

El archivo volcado te enseña **la capa que importa** — lo que añade `#[atomic2fmu]`. Por
debajo hay dos capas más (infraestructura de `rust-fmi`, común a toda FMU):

```
tu modelo DEVS
   │  ← #[atomic2fmu]        → el wrapper   ← ESTO es lo que se vuelca a target/atomic2fmu/
   │  ← #[derive(FmuModel)]  → impl Model, get/set, y el enum <Fmu>ValueRef
   │  ← export_fmu!          → los símbolos C de FMI (el ABI)
   ▼
código FMI completo  →  cargo fmi bundle  →  .fmu
```

## Ver también las dos capas de abajo (expansión total, opcional)

Si quieres el código COMPLETO tras TODAS las macros (incluidas las dos de abajo), necesita
*nightly* + `cargo-expand`:

```bash
rustup toolchain install nightly
cargo install cargo-expand
cd semaforo_mod_se
cargo +nightly expand > EXPANDIDO.rs
```

Para el seguimiento del día a día basta con `target/atomic2fmu/<Fmu>.rs`.
