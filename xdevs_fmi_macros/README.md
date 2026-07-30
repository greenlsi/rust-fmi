# `xdevs_fmi_macros` — la macro `#[atomic2fmu]`

Genera el envoltorio FMI 3.0 (Scheduled Execution) de un modelo DEVS **atómico** de
[xdevs](https://github.com/iscar-ucm/xdevs_no_std.rs), sobre el adaptador
[`xdevs_fmi`](../xdevs_fmi). Se re-exporta desde ahí, así que se usa como
`#[xdevs_fmi::atomic2fmu(...)]`.

La macro genera todo el pegamento: el reloj *countdown* `ta`, un reloj *triggered* por
cada entrada, el campo interno `sim`, el `Default`, la `impl UserModel` (con el `match` de
`activate_partition`, las closures de captura y `next_interval`) y el `export_fmu!`.

## Dos formas

### Forma módulo (recomendada) — la macro LEE el modelo

El modelo DEVS vive **dentro** del módulo. La macro recibe todos sus tokens, encuentra el
`impl xdevs::Component`, saca los tipos de los puertos y, si la salida es un `enum` del
módulo, genera una variable `Float64` one-hot por variante.

```rust
#[xdevs_fmi::atomic2fmu(init = Semaphore::new(Duration::from_secs(30), Duration::from_secs(15)))]
mod semaforo_fmu {
    use xdevs::{Duration, Port};

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum SemaphoreState { Green, Red }

    pub struct Semaphore { /* ... */ }
    impl xdevs::Component for Semaphore {
        type Input = Port<bool, 1>;
        type Output = Port<SemaphoreState, 1>;
        type Kind = xdevs::AtomicKind;
    }
    impl xdevs::Atomic for Semaphore { /* ... */ }
    impl Semaphore { pub fn new(r: Duration, g: Duration) -> Self { /* ... */ } }
}
```

Genera una FMU con: `ta` (countdown), `input_ev` (triggered) + `input` (`Boolean`, de
`Port<bool,1>`), y `green`/`red` (`Float64` one-hot, del `enum`). Solo se pasa `init` (o el
modelo implementa `Default`).

**Requisito:** el modelo tiene que estar **textualmente dentro del módulo** — la macro solo
ve tokens literales (ni un `use` a un crate compilado ni un `include!` sirven).

### Forma struct — el modelo está en otro crate

Cuando no puedes meter el modelo en el módulo, declaras la interfaz y la macro la comprueba
contra `<Model as Component>::Input/Output` (si te equivocas de tipo, no compila).

```rust
#[xdevs_fmi::atomic2fmu(
    model = DutyCycleCalculator,
    init  = DutyCycleCalculator::new(Duration::from_secs(20), false),
)]
struct DutyCycleCalculatorFmu {
    #[input]  entrada: bool,      // → reloj triggered + Boolean
    #[output] duty_medido: f64,   // → Float64 (identidad: mismo tipo que el puerto)
}
```

Etiquetas de campo (forma struct):

| Etiqueta | Efecto |
| --- | --- |
| `#[input] n: T` | reloj triggered `n_ev` + variable `T` clocked |
| `#[output] n: T` | salida del puerto (identidad si `T` = tipo del evento) |
| `#[output(from = \|ev\| ...)] n: T` | salida con conversión de tipo |
| `#[output(ta)] n: f64` | salida = `sim.ta()` (el σ restante) |
| `... start = ...` | valor inicial (por defecto `false`/`0`/`0.0` según el tipo) |

## Seguimiento: el código generado se vuelca en cada build

En cada compilación la macro escribe el "código FMI" que genera (el wrapper) en:

```
<crate>/target/atomic2fmu/<NombreDeLaFmu>.rs
```

Formateado y legible, para seguir el paso modelo DEVS → FMI sin instalar nada. Se
desactiva con `ATOMIC2FMU_NO_DUMP=1`. (Para la expansión COMPLETA, con las capas de
`#[derive(FmuModel)]` y `export_fmu!`, usa `cargo +nightly expand`.)

## Alcance — N entradas / M salidas

Modelos **atómicos** (vía `DevsFmu::atomic`). La macro inspecciona `type Input`/`type
Output` y se adapta a la forma que sea:

| Tipo de la bolsa | Puertos |
| --- | --- |
| `Port<T,N>` | 1 |
| `()` | 0 |
| `(Port<A>, Port<B>, …)` (tupla) | N, acceso `.0`/`.1` |
| `[Port<T>; K]` (array) | K, acceso `[i]` |
| struct con `#[derive(xdevs::Bag)]` | 1 por campo, acceso `.campo` |

Ejemplos: `semaforo_mod_se` (1 in / 1 out enum→one-hot), `dual_sensor_se` (**2 in / 2
out** con structs de puertos).

## Modelos acoplados: `#[coupled2fmu]`

La macro hermana `#[coupled2fmu]` envuelve un modelo **acoplado** (`impl xdevs::Coupled`,
normalmente con `#[xdevs::coupled]`). Es idéntica salvo que conduce el modelo con un
`Coordinator` (`DevsFmu::coupled`) y comprueba `impl Coupled`. Los puertos externos del
acoplado se exponen igual (N/M). Ejemplo: `pwm_duty_coupled_se` — un generador PWM
acoplado a un medidor de duty, todo dentro de una sola FMU (salida `output` = el duty
medido). La red interna del acoplado se resuelve dentro de la FMU.

## Licencia

GPL-3.0-or-later (enlaza con `xdevs-no-std`, que es GPL).
