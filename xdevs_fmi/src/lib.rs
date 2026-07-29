//! **Adaptador genérico xdevs → FMI 3.0 Scheduled Execution.**
//!
//! Envolver un modelo DEVS como FMU tiene una parte mecánica (el protocolo de
//! simulación: cuándo va λ, cuándo δ, cuánto tiempo ha pasado, cuál es el próximo
//! evento) y una parte que es una **decisión de modelado** (qué variable FMI
//! representa cada puerto DEVS). Este crate resuelve la primera de una vez por todas;
//! la segunda no se puede automatizar y la escribes tú.
//!
//! # Vale para átomos Y para acoplados
//!
//! El adaptador trabaja sobre [`AbstractSimulator`], que es el protocolo DEVS que
//! implementan tanto `Simulator<T>` (un átomo) como los coordinadores que genera la
//! macro `coupled!` de xdevs. El mismo código sirve para los dos casos:
//!
//! ```ignore
//! let sim = DevsFmu::atomic(MiAtomo::new(...));   // átomo
//! let sim = DevsFmu::new(mi_acoplado);            // acoplado ya convertido
//! ```
//!
//! # El mapeo a FMI
//!
//! | DEVS | FMI 3.0 |
//! | --- | --- |
//! | `ta()` | reloj **countdown** → [`DevsFmu::ta`] en `fmi3GetIntervalDecimal` |
//! | evento interno (λ + δint) | `fmi3ActivateModelPartition(reloj_ta, t)` → [`DevsFmu::step`] sin llenar la bolsa |
//! | evento externo (δext) | `fmi3ActivateModelPartition(reloj_ev, t)` → [`DevsFmu::step`] llenando la bolsa |
//!
//! Un único método, [`DevsFmu::step`], cubre los dos relojes, y además resuelve bien
//! el caso **confluente** (δconf) porque reproduce literalmente el bucle de referencia
//! de xdevs: si el instante coincide con el próximo evento interno, llama a λ antes de
//! δ, y entonces `delta` despacha `delta_conf` en vez de `delta_ext`.
//!
//! # Lo que NO puede hacer por ti
//!
//! - **Elegir la representación FMI de los puertos.** Un `Port<bool,1>` es una entrada
//!   `Boolean` evidente; un `Port<MiEnum,1>` necesita que decidas su codificación.
//! - **Bolsas con más de un evento por activación.** Una variable *clocked* de FMI lleva
//!   un valor por tick. Si tu modelo mete varios eventos en la bolsa en el mismo
//!   instante, hay que decidir cómo se transporta (un reloj por puerto, o una variable
//!   array). Ver `COMO_ENVOLVER.md`.
//! - **Leer estado privado.** Si los campos del modelo son privados, las salidas de la
//!   FMU se derivan de lo que emite λ.
//! - **Exponer argumentos del constructor como parámetros FMI**, salvo que el modelo
//!   ofrezca `Default` o setters públicos.

use xdevs::{
    port::Bag,
    simulation::{AbstractSimulator, Simulable},
    Atomic, Coupled, Instant,
};

// Re-exportados para que un envoltorio no necesite depender de xdevs por su cuenta:
// con `xdevs_fmi` basta para escribir el tipo del campo y construir el modelo.
pub use xdevs::simulation::coordinator::Coordinator;
pub use xdevs::simulation::simulator::Simulator;
pub use xdevs::{Atomic as AtomicModel, Component, Duration, Port};

/// **La macro `#[atomic2fmu(...)]`**: genera el wrapper FMI completo de un modelo DEVS
/// atómico de xdevs (reloj `ta`, reloj triggered por entrada, campo `sim`, `Default`,
/// `impl UserModel` y `export_fmu!`). Dos formas:
///
/// **Forma módulo (recomendada)** — el modelo vive dentro del módulo y la macro lo LEE
/// (puertos del `impl Component`, variantes del `enum` de salida → one-hot). Solo hace
/// falta el `init` (o que el modelo implemente `Default`):
///
/// ```ignore
/// #[xdevs_fmi::atomic2fmu(init = Semaphore::new(Duration::from_secs(30), Duration::from_secs(15)))]
/// mod semaforo_fmu {
///     // ...el modelo DEVS entero (struct + impl Component + impl Atomic + enum)...
/// }
/// ```
///
/// **Forma struct** — cuando el modelo está en otro crate; declaras la interfaz y la
/// macro la comprueba contra `<Model as Component>::Input/Output`:
///
/// ```ignore
/// #[xdevs_fmi::atomic2fmu(model = DutyCycleCalculator, init = DutyCycleCalculator::new(...))]
/// struct DutyCycleCalculatorFmu {
///     #[input]  entrada: bool,
///     #[output] duty_medido: f64,
/// }
/// ```
///
/// Etiquetas (forma struct): `#[input] n: T`; `#[output] n: T` (identidad);
/// `#[output(from = |ev| ...)]` (conversión); `#[output(ta)] n: f64` (= `sim.ta()`);
/// `start = ...` opcional.
pub use xdevs_fmi_macros::atomic2fmu;

/// Intervalo que se reporta cuando el modelo queda **pasivo** (`ta() = ∞`).
///
/// FMI no tiene forma de decir "infinito" en un reloj countdown, así que se devuelve
/// un valor enorme pero finito (~31.700 años) que en la práctica no se alcanza y no
/// rompe la aritmética del planificador.
pub const INTERVALO_PASIVO: f64 = 1e12;

/// Segundos (coma flotante) → `Instant` de xdevs.
#[inline]
pub fn a_instant(segundos: f64) -> Instant {
    Instant::from_micros((segundos.max(0.0) * 1e6).round() as u64)
}

/// `Instant` de xdevs → segundos.
#[inline]
pub fn a_segundos(t: Instant) -> f64 {
    t.as_micros() as f64 / 1e6
}

/// Conduce un simulador DEVS desde marcas de tiempo absolutas en segundos.
///
/// Mantiene las bolsas de entrada/salida y el instante del próximo evento interno,
/// que es lo que alimenta al reloj *countdown* de la FMU.
pub struct DevsFmu<S: AbstractSimulator> {
    sim: S,
    entrada: S::Input,
    salida: S::Output,
    /// Instante del próximo evento interno (lo devuelve `start`/`delta`).
    t_next: Instant,
    /// Instante de la última llamada a [`step`](DevsFmu::step); `ta()` se mide desde aquí.
    t_actual: Instant,
}

impl<M: Atomic> DevsFmu<Simulator<M>> {
    /// Envuelve un **modelo atómico** (el caso habitual).
    pub fn atomic(modelo: M) -> Self {
        Self::new(modelo.to_simulator())
    }
}

impl<C: Coupled> DevsFmu<Coordinator<C>> {
    /// Envuelve un **modelo acoplado** (el construido con `#[coupled]`).
    ///
    /// La red interna se resuelve dentro de la FMU: desde fuera es una caja negra con
    /// sus puertos. El resto del envoltorio es idéntico al de un átomo — de eso trata
    /// que el adaptador esté escrito sobre [`AbstractSimulator`] y no sobre `Atomic`.
    pub fn coupled(modelo: C) -> Self {
        Self::new(modelo.to_simulator())
    }
}

impl<S: AbstractSimulator> DevsFmu<S> {
    /// Envuelve un simulador ya construido (un átomo con `to_simulator()`, o el
    /// coordinador de un modelo **acoplado**).
    ///
    /// Llama a `start(0)`, que es lo que fija el primer `ta()`.
    pub fn new(mut sim: S) -> Self {
        let t0 = Instant::from_secs(0);
        let t_next = sim.start(t0);
        Self {
            sim,
            entrada: <S::Input>::build(),
            salida: <S::Output>::build(),
            t_next,
            t_actual: t0,
        }
    }

    /// Un paso del protocolo DEVS en el instante absoluto `t` (segundos).
    ///
    /// Reproduce el bucle de referencia de xdevs (`AbstractSimulator::simulate_rt`):
    ///
    /// 1. `llenar` mete en la bolsa de entrada los eventos externos (puede no meter
    ///    ninguno: eso es un evento puramente interno).
    /// 2. Si `t` alcanza el próximo evento interno, se ejecuta **λ con el estado
    ///    previo** y se te entrega la bolsa de salida en `leer` — es tu única
    ///    oportunidad de leerla, porque `delta` la limpia justo después.
    /// 3. `delta` despacha δint, δext o **δconf** según corresponda y reprograma el
    ///    próximo evento interno.
    ///
    /// Si no toca evento interno y la bolsa de entrada quedó vacía, no hace nada
    /// (evita transiciones externas espurias).
    pub fn step(
        &mut self,
        t: f64,
        llenar: impl FnOnce(&mut S::Input),
        leer: impl FnOnce(&S::Output),
    ) {
        let ti = a_instant(t);
        self.t_actual = ti;
        self.salida.clear();
        llenar(&mut self.entrada);

        if ti >= self.t_next {
            // λ SIEMPRE antes que δ: en DEVS la salida se calcula con el estado previo.
            self.sim.lambda(&mut self.salida, ti);
            leer(&self.salida);
        } else if self.entrada.is_empty() {
            return; // ni evento interno ni externo: nada que hacer
        }

        self.t_next = self.sim.delta(&mut self.entrada, &mut self.salida, ti);
    }

    /// Segundos hasta el próximo evento interno, **medidos desde la última llamada a
    /// [`step`](DevsFmu::step)** — justo lo que pide `fmi3GetIntervalDecimal` de un
    /// reloj *countdown*, que el maestro consulta después de cada activación.
    ///
    /// Si el modelo quedó pasivo devuelve [`INTERVALO_PASIVO`].
    pub fn ta(&self) -> f64 {
        let restante = a_segundos(self.t_next) - a_segundos(self.t_actual);
        if !restante.is_finite() || restante > INTERVALO_PASIVO {
            INTERVALO_PASIVO
        } else {
            restante.max(0.0)
        }
    }

    /// Instante absoluto del próximo evento interno, en segundos.
    pub fn t_proximo(&self) -> f64 {
        a_segundos(self.t_next)
    }

    /// Acceso de solo lectura al simulador (y, vía `Deref`, al modelo).
    pub fn simulador(&self) -> &S {
        &self.sim
    }

    /// Cierre ordenado (`stop()` del modelo). Llamar desde `fmi3Terminate`.
    pub fn stop(&mut self) {
        self.sim.stop();
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Capa 2: la traducción de tipos DEVS ↔ variables FMI
// ═════════════════════════════════════════════════════════════════════════════

/// Cómo se representa en FMI el dato que viaja por un puerto DEVS.
///
/// FMI solo conoce `Float64`, `Int32`, `Boolean`, `String` y `Binary`; un modelo DEVS
/// manda `MiEnum`. Este trait fija la traducción **una sola vez por tipo**, y a partir
/// de ahí el envoltorio no tiene que repetirla.
///
/// Para los tipos primitivos ya está implementado. Para un `enum` sin campos, la
/// macro [`carga_fmi_enum!`] genera la implementación.
///
/// Si el enum es **de otro crate** (el caso normal al envolver un modelo ajeno) no se
/// puede implementar el trait directamente sobre él: lo impide la *orphan rule* de
/// Rust, porque ni el trait ni el tipo son tuyos. La macro tiene una forma que genera
/// un *newtype* local y le pone la implementación; sale igual de corto.
pub trait CargaFmi: Copy {
    /// Nombre de cada variante, en orden. Sirve para nombrar las variables FMI.
    const VARIANTES: &'static [&'static str];

    /// Código entero de esta variante → una variable FMI `Int32`.
    fn codigo(&self) -> i32;

    /// Vuelta atrás: de código a variante.
    fn desde_codigo(codigo: i32) -> Option<Self>;

    /// Representación *one-hot*: una variable `Float64` por variante, a 1.0 la activa.
    ///
    /// Es la que suele querer un ingeniero, porque cada variante se grafica sola
    /// (`rojo`, `verde`, …) en vez de tener que interpretar un código.
    fn una_por_variante(&self, salida: &mut [f64]) {
        let c = self.codigo();
        for (i, v) in salida.iter_mut().enumerate().take(Self::VARIANTES.len()) {
            *v = if i as i32 == c { 1.0 } else { 0.0 };
        }
    }

    /// Cuántas variables FMI hacen falta en la representación *one-hot*.
    fn n_variables() -> usize {
        Self::VARIANTES.len()
    }
}

/// Implementa [`CargaFmi`] para un `enum` sin campos. Dos formas:
///
/// **Enum tuyo** (está en tu crate):
///
/// ```ignore
/// xdevs_fmi::carga_fmi_enum! { MiFase { Parado, Marcha } }
/// ```
///
/// **Enum de otro crate** — genera un *newtype* local, porque la *orphan rule* no deja
/// implementar un trait ajeno sobre un tipo ajeno:
///
/// ```ignore
/// xdevs_fmi::carga_fmi_enum! {
///     Fase => devs_semaphore::SemaphoreState { Red, Green }
/// }
/// // Fase(SemaphoreState) con From<…>, Deref y CargaFmi ya implementados.
/// ```
///
/// El orden que escribas fija los códigos (0, 1, 2…) y el orden de las variables
/// *one-hot*: es parte del contrato público de tu FMU, así que no lo cambies sin
/// avisar a quien la conecte.
///
/// El enum tiene que ser `Copy + PartialEq` (lo normal en un enum sin campos).
#[macro_export]
macro_rules! carga_fmi_enum {
    // ── Enum propio: implementación directa ──────────────────────────────────
    ($tipo:ident { $($variante:ident),+ $(,)? }) => {
        impl $crate::CargaFmi for $tipo {
            const VARIANTES: &'static [&'static str] = &[$(stringify!($variante)),+];

            fn codigo(&self) -> i32 {
                let mut _i = 0i32;
                $(
                    if *self == <$tipo>::$variante { return _i; }
                    _i += 1;
                )+
                -1
            }

            fn desde_codigo(codigo: i32) -> Option<Self> {
                let mut _i = 0i32;
                $(
                    if codigo == _i { return Some(<$tipo>::$variante); }
                    _i += 1;
                )+
                None
            }
        }
    };

    // ── Enum ajeno: newtype local + implementación ───────────────────────────
    ($nuevo:ident => $tipo:ty { $($variante:ident),+ $(,)? }) => {
        #[doc = concat!("Envoltura local de `", stringify!($tipo), "` para poder darle una representación FMI.")]
        #[derive(Debug, Clone, Copy, PartialEq)]
        pub struct $nuevo(pub $tipo);

        impl From<$tipo> for $nuevo {
            fn from(v: $tipo) -> Self { Self(v) }
        }

        impl ::core::ops::Deref for $nuevo {
            type Target = $tipo;
            fn deref(&self) -> &Self::Target { &self.0 }
        }

        impl $crate::CargaFmi for $nuevo {
            const VARIANTES: &'static [&'static str] = &[$(stringify!($variante)),+];

            fn codigo(&self) -> i32 {
                let mut _i = 0i32;
                $(
                    if self.0 == <$tipo>::$variante { return _i; }
                    _i += 1;
                )+
                -1
            }

            fn desde_codigo(codigo: i32) -> Option<Self> {
                let mut _i = 0i32;
                $(
                    if codigo == _i { return Some($nuevo(<$tipo>::$variante)); }
                    _i += 1;
                )+
                None
            }
        }
    };
}

macro_rules! carga_fmi_bool {
    () => {
        impl CargaFmi for bool {
            const VARIANTES: &'static [&'static str] = &["false", "true"];
            fn codigo(&self) -> i32 {
                *self as i32
            }
            fn desde_codigo(c: i32) -> Option<Self> {
                match c {
                    0 => Some(false),
                    1 => Some(true),
                    _ => None,
                }
            }
        }
    };
}
carga_fmi_bool!();

#[cfg(test)]
mod tests {
    use super::*;
    use xdevs::{Component, Duration, Port};

    /// Átomo de prueba: emite un contador cada `periodo`; un evento externo con
    /// `true` adelanta el próximo evento interno a la mitad.
    struct Contador {
        sigma: Duration,
        periodo: Duration,
        n: u32,
    }
    impl Component for Contador {
        type Input = Port<bool, 1>;
        type Output = Port<u32, 1>;
        type Kind = xdevs::AtomicKind;
    }
    impl Atomic for Contador {
        fn delta_int(&mut self) {
            self.n += 1;
            self.sigma = self.periodo;
        }
        fn delta_ext(&mut self, e: Duration, input: &Self::Input) {
            self.sigma -= e;
            if input.get_values().last() == Some(&true) {
                self.sigma = self.sigma / 2;
            }
        }
        fn lambda(&self, output: &mut Self::Output) {
            output.add_value(self.n + 1).unwrap();
        }
        fn ta(&self) -> Duration {
            self.sigma
        }
    }
    fn contador() -> DevsFmu<Simulator<Contador>> {
        DevsFmu::atomic(Contador {
            sigma: Duration::from_secs(10),
            periodo: Duration::from_secs(10),
            n: 0,
        })
    }

    #[test]
    fn el_primer_ta_sale_de_start() {
        let c = contador();
        assert_eq!(c.ta(), 10.0);
        assert_eq!(c.t_proximo(), 10.0);
    }

    #[test]
    fn evento_interno_emite_lambda_y_reprograma() {
        let mut c = contador();
        let mut visto = None;
        c.step(10.0, |_| {}, |out| visto = out.get_values().last().copied());
        assert_eq!(visto, Some(1), "λ se ejecuta con el estado PREVIO");
        assert_eq!(c.ta(), 10.0, "reprogramado a t=20");
        assert_eq!(c.t_proximo(), 20.0);
    }

    /// Antes del próximo evento interno y con la bolsa vacía no debe pasar nada:
    /// es la protección contra transiciones externas espurias.
    #[test]
    fn sin_entrada_y_antes_de_tiempo_no_hace_nada() {
        let mut c = contador();
        let mut lambda_llamada = false;
        c.step(3.0, |_| {}, |_| lambda_llamada = true);
        assert!(!lambda_llamada);
        assert_eq!(c.t_proximo(), 10.0, "el próximo evento no se movió");
    }

    #[test]
    fn evento_externo_reprograma_el_interno() {
        let mut c = contador();
        // En t=4 llega `true`: sigma = (10-4)/2 = 3 → próximo interno en t=7.
        c.step(4.0, |inp| { inp.add_value(true).unwrap(); }, |_| {});
        assert_eq!(c.ta(), 3.0, "ta() se mide desde el instante actual");
        assert_eq!(c.t_proximo(), 7.0);
    }

    /// Un δext que NO cambia sigma no debe mover el instante del próximo evento:
    /// `ta()` baja porque el tiempo pasa, pero `t_proximo()` se queda igual.
    #[test]
    fn un_externo_neutro_no_mueve_el_proximo_evento() {
        let mut c = contador();
        c.step(4.0, |inp| { inp.add_value(false).unwrap(); }, |_| {});
        assert_eq!(c.t_proximo(), 10.0, "sigue en t=10");
        assert_eq!(c.ta(), 6.0, "pero quedan 6 s desde t=4");
    }

    /// Evento externo e interno en el MISMO instante → δconf, y λ se ejecuta.
    /// Es el caso que un envoltorio hecho a mano suele resolver mal.
    // ── Capa 2: traducción de tipos ──────────────────────────────────────────

    /// Un enum "de otro crate" (aquí simulado) al que NO se le puede poner un
    /// `#[derive]`: la macro tiene que funcionar igual.
    #[derive(Debug, Clone, Copy, PartialEq)]
    enum Fase {
        Parado,
        Acelerando,
        Crucero,
    }
    crate::carga_fmi_enum! { Fase { Parado, Acelerando, Crucero } }

    #[test]
    fn el_enum_se_traduce_a_codigo_y_vuelve() {
        assert_eq!(Fase::Parado.codigo(), 0);
        assert_eq!(Fase::Acelerando.codigo(), 1);
        assert_eq!(Fase::Crucero.codigo(), 2);
        assert_eq!(Fase::desde_codigo(2), Some(Fase::Crucero));
        assert_eq!(Fase::desde_codigo(7), None);
    }

    #[test]
    fn el_enum_da_una_variable_por_variante() {
        assert_eq!(Fase::n_variables(), 3);
        assert_eq!(Fase::VARIANTES, &["Parado", "Acelerando", "Crucero"]);
        let mut v = [0.0; 3];
        Fase::Acelerando.una_por_variante(&mut v);
        assert_eq!(v, [0.0, 1.0, 0.0], "one-hot: solo la variante activa a 1.0");
    }

    #[test]
    fn los_primitivos_ya_vienen_implementados() {
        assert_eq!(true.codigo(), 1);
        assert_eq!(bool::desde_codigo(0), Some(false));
    }

    #[test]
    fn coincidencia_exacta_dispara_confluente_con_lambda() {
        let mut c = contador();
        let mut visto = None;
        c.step(10.0,
            |inp| { inp.add_value(true).unwrap(); },
            |out| visto = out.get_values().last().copied());
        assert_eq!(visto, Some(1), "λ debe ejecutarse también en el confluente");
        // delta_conf = delta_int (sigma=10) y luego delta_ext(e=0) → 10/2 = 5.
        assert_eq!(c.ta(), 5.0);
    }
}
