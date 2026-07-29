# Cómo convertir un modelo xdevs en una FMU

> Receta completa. Si alguien te pasa un modelo DEVS escrito con `xdevs-no-std`, esto es
> lo que hay que hacer para que corra bajo FMI 3.0 Scheduled Execution, acoplado a
> cualquier otra FMU (sea DEVS o no).

## Lo primero: qué es automático y qué no

Envolver un DEVS tiene **tres capas**, y solo la última es trabajo tuyo:

| Capa | ¿Automática? | Quién la pone |
| --- | --- | --- |
| **1. Protocolo de simulación** — λ antes de δ, δint/δext/**δconf**, el `e` transcurrido, el próximo evento, limpiar bolsas | ✅ **Sí, del todo** | `xdevs_fmi::DevsFmu`, igual para atómicos y acoplados |
| **2. Traducción de tipos** — `MiEnum` ↔ variables FMI | ✅ **Sí**, una línea | `carga_fmi_enum!` + el trait `CargaFmi` |
| **3. Interfaz de la FMU** — qué variables hay, cómo se llaman, cuál lleva cada puerto | ❌ **No** | tú |

La capa 3 **no es un límite técnico, es una especificación**: son los nombres y tipos que
verá quien conecte tu FMU, o sea su contrato público. Nadie puede inferir que tu enum de
4 estados debe llamarse `fase` y ser un `Int32`, o cuatro `Boolean` con nombres propios.
Igual que un `.h` en C: el compilador no lo adivina, lo escribes.

En el ejemplo de referencia son **unas 30 líneas**: la declaración de variables y dos
closures.

### La capa 2, en concreto

```rust
// Enum tuyo:
xdevs_fmi::carga_fmi_enum! { MiFase { Parado, Marcha, Frenado } }

// Enum de otro crate (genera un newtype local: la orphan rule no deja otra cosa):
xdevs_fmi::carga_fmi_enum! { Fase => devs_semaphore::SemaphoreState { Red, Green } }
```

y a partir de ahí tienes las dos representaciones habituales sin escribirlas:

```rust
Fase(color).codigo()                      // → i32, para una variable Int32
Fase(color).una_por_variante(&mut flags); // → one-hot, para `rojo`/`verde`/… en Float64
Fase::VARIANTES                           // → ["Red", "Green"], para nombrar las variables
```

---

## Los pasos

### 1. Crear el crate del envoltorio

```toml
[package]
name = "mi_modelo_fmu"
edition = "2021"
# xdevs-no-std es GPL-3.0-or-later y esto enlaza con él → la FMU resultante es GPL.
license = "GPL-3.0-or-later"

[lib]
crate-type = ["cdylib", "rlib"]

[dependencies]
fmi        = { path = "../rust-fmi/fmi",        features = ["fmi3"], default-features = false }
fmi-export = { path = "../rust-fmi/fmi-export", features = ["fmi3"] }
mi_modelo  = { path = "../ruta/al/modelo" }   # ← SIN modificar
xdevs_fmi  = { path = "../xdevs_fmi" }

[package.metadata.fmu]
default_experiment = { start_time = "0", stop_time = "200", step_size = "0.5" }
```

### 2. Mirar los puertos del modelo

```rust
impl xdevs::Component for MiModelo {
    type Input  = Port<bool, 1>;             // ← qué entra
    type Output = Port<MiEstado, 1>;         // ← qué sale
    type Kind   = xdevs::AtomicKind;
}
```

### 3. Decidir el mapeo (la única parte creativa)

| Tipo del puerto DEVS | Variable FMI razonable |
| --- | --- |
| `bool` | `Boolean` relojado |
| números | `Float64` / `Int32` relojado |
| `enum` de pocos valores | un `Float64`/`Boolean` por valor (como `rojo`/`verde`), o un `Int32` con el discriminante |
| `struct` | una variable por campo |
| algo grande o variable | `Binary` con tu propia serialización |

**Regla:** toda variable de dato lleva `clocks = [reloj]`, porque solo es válida cuando
su reloj está activo. Eso es lo que separa *que ha ocurrido un evento* de *qué dato
lleva*, y es justo lo que hace que dos eventos seguidos con el mismo valor sean dos
eventos y no uno.

### 4. Declarar el struct de la FMU

Dos relojes: uno *countdown* para `ta()` y uno *triggered* por cada puerto de entrada.

```rust
pub const VR_TA: fmi3ValueReference = 1;
pub const VR_EV: fmi3ValueReference = 2;

#[derive(FmuModel)]
#[model(model_exchange = false, co_simulation = false,
        scheduled_execution = true, user_model = false)]
pub struct MiModeloFmu {
    #[variable(causality = Input, interval_variability = Countdown)]
    ta: Clock,                                    // el ta() de DEVS
    #[variable(causality = Input, interval_variability = Triggered)]
    ev: Clock,                                    // el evento externo

    #[variable(causality = Input, variability = Discrete, start = false, clocks = [ev])]
    dato_entrada: bool,                           // la bolsa de entrada
    #[variable(causality = Output, variability = Discrete, start = 0.0, clocks = [ta])]
    dato_salida: f64,                             // la bolsa de salida

    // Estado interno: sin atributos → invisible para el derive
    sim: DevsFmu<Simulator<MiModelo>>,
    // Si los campos del modelo son privados, guarda aquí lo que emita λ
}
```

Los VR salen del **orden de los campos** (empezando en 1, porque `time` es el 0).
Compruébalos con `cargo fmi inspect`.

### 5. Los tres métodos

```rust
impl UserModel for MiModeloFmu {
    type LoggingCategory = DefaultLoggingCategory;

    // Estado interno → variables de salida FMI.
    fn calculate_values(&mut self, _c: &dyn Context<Self>) -> Result<Fmi3Res, Fmi3Error> {
        self.dato_salida = /* … */;
        Ok(Fmi3Res::OK)
    }

    fn activate_partition(&mut self, _c: &mut dyn Context<Self>,
                          clock: fmi3ValueReference, t: f64) -> Result<Fmi3Res, Fmi3Error> {
        // La bolsa de salida SOLO existe entre λ y δ: captúrala aquí.
        let mut emitido = None;
        match clock {
            VR_TA => self.sim.step(t, |_| {},                       // sin entrada
                                   |out| emitido = out.get_values().last().copied()),
            VR_EV => {
                let dato = self.dato_entrada;
                self.sim.step(t, |inp| { let _ = inp.add_value(dato); },
                              |out| emitido = out.get_values().last().copied());
            }
            _ => return Err(Fmi3Error::Error),
        }
        if let Some(v) = emitido { /* guardar */ }
        Ok(Fmi3Res::OK)
    }

    fn next_interval(&self, clock: fmi3ValueReference) -> Option<f64> {
        match clock {
            VR_TA => Some(self.sim.ta()),
            _     => None,          // un triggered NO tiene intervalo
        }
    }
}
fmi_export::export_fmu!(MiModeloFmu);
```

`None` es importante: se traduce a `fmi3IntervalNotYetKnown`, que es lo correcto para un
reloj *triggered*. Si devolvieras un número, el planificador creería que el evento
externo es periódico.

### 6. Empaquetar y comprobar

```bash
cargo test          # la semántica del modelo, sin FMI de por medio
cargo fmi bundle    # → target/fmu/mi_modelo_fmu.fmu
cargo fmi inspect target/fmu/mi_modelo_fmu.fmu --format model-description
```

En el XML tienen que salir los dos `<Clock>` (uno `countdown`, otro `triggered`) y el
atributo `clocks="…"` en todas las variables de dato.

---

## Modelos acoplados

`DevsFmu` trabaja sobre `AbstractSimulator`, que también implementan los coordinadores
que genera la macro `coupled!` de xdevs. Así que un modelo acoplado se envuelve igual,
cambiando solo la construcción:

```rust
sim: DevsFmu<Simulator<MiAtomo>>,        // atómico
sim: DevsFmu::atomic(MiAtomo::new(…)),

sim: DevsFmu<MiAcopladoCoordinador>,     // acoplado
sim: DevsFmu::new(mi_acoplado),
```

La red interna se resuelve **dentro** de la FMU: desde fuera es una caja negra con sus
puertos. Los pasos 3-5 son idénticos. **Probado de punta a punta** en
[`semaforo_cooldown_se/`](../semaforo_cooldown_se/src/lib.rs): un `Cooldown` propio
delante del `Semaphore` de Dani, empaquetados como una sola FMU y co-simulados contra el
generador de botón.

Dos cosas que conviene saber del caso acoplado:

- **`ta()` es el mínimo de los componentes.** `sim.t_proximo()` ya no te dice cuándo
  cambia un componente concreto, sino cuál es el primer evento de toda la red. Si tu
  test comprueba tiempos, compruébalos por su efecto observable.
- **Transiciones instantáneas (σ = 0).** Un componente puede emitir y transitar sin que
  avance el tiempo (tiempo super-denso de DEVS). El maestro tiene que reactivar el reloj
  *countdown* en el mismo instante; el planificador de SeRo_CoSim lo hace, reeligiendo el
  reloj más próximo en cada vuelta con un tope de seguridad.

---

## Limitaciones reales — léelas antes de prometer nada

**1. Bolsas con más de un evento por activación.** Una variable *clocked* de FMI lleva
**un** valor por tick. Si en un mismo instante salen varios eventos, hay que repartirlos.
La receta que funciona, y que usa `semaforo_cooldown_se`, es **una variable FMI por
variante**: su salida es `Port<EventoSalida, 2>` (puede salir un cambio de color y una
pulsación aceptada a la vez) y se reparte en `rojo`/`verde` + `pulsos_aceptados`. Para
bolsas de tamaño variable del mismo tipo (N eventos iguales) no hay equivalente directo:
toca una variable array o `Binary` serializado.

**2. Tipos compuestos.** Para `enum` sin campos lo resuelve `carga_fmi_enum!`. Para
`struct` con datos sigue siendo manual: una variable FMI por campo. Y en cualquier caso
**la codificación es parte del contrato de tu FMU**: documéntala para quien la conecte.

**3. Estado privado.** Si los campos del modelo son privados no puedes publicarlos como
salidas FMI: solo puedes exponer lo que emita λ. En el semáforo eso basta porque λ emite
el color al que transita, pero no siempre será así.

**4. Parámetros del constructor.** Si el modelo recibe su configuración por
`new(...)` y guarda los campos privados, **no puedes exponerlos como parámetros FMI**
sin construir el modelo en `configurate` (que corre después de fijar los parámetros) —
y para eso el modelo tiene que ofrecer `Default` o setters.

**5. Confluencia repartida.** El adaptador resuelve bien δconf cuando el evento externo
cae exactamente en el instante del interno *dentro de la misma activación*. Pero el
maestro puede activar el reloj *triggered* y el *countdown* como dos llamadas separadas
en el mismo timestamp; entonces se ejecuta δext y después lo que diga el nuevo `ta()`,
que no siempre es idéntico a δconf. Si tu modelo distingue δconf de "δext y luego δint",
hay que revisarlo.

**6. Licencia.** `xdevs-no-std` es **GPL-3.0-or-later**. Cualquier FMU que enlace un
modelo xdevs es un derivado GPL. Tenlo claro antes de distribuir el `.fmu`.

---

## Ejemplo completo

[`xdevs_semaforo_se/`](../xdevs_semaforo_se/src/lib.rs) — el `Semaphore` de
`devs_semaphore` sin tocar una línea, con `Port<bool,1>` → `Boolean boton` y
`Port<SemaphoreState,1>` → `rojo`/`verde`. Y en el orquestador,
`proyectos/se_xdevs_semaforo.sero` lo acopla a un generador que es una FMU de
Co-Simulation corriente.
