//! **Demo de `#[atomic2fmu]` en FORMA MÓDULO.**
//!
//! Aquí el modelo DEVS vive **dentro** del módulo, y la macro lo LEE: encuentra el
//! `impl xdevs::Component`, saca los tipos de los puertos (`Input = Port<bool,1>`,
//! `Output = Port<SemaphoreState,1>`), ve que la salida es el `enum SemaphoreState` del
//! módulo y genera una variable one-hot por variante (`green`, `red`). Lo único que se
//! le pasa es el `init` (los periodos del constructor).
//!
//! Es exactamente el modelo de Dani, **sin una sola línea cambiada**, pegado en el
//! módulo. La macro añade al lado la FMU (`SemaphoreFmu`) con su reloj `ta`, su reloj
//! triggered `input_ev`, la entrada `input`, las salidas `green`/`red`, el `Default`, la
//! `impl UserModel` y el `export_fmu!`.

#[xdevs_fmi::atomic2fmu(init = Semaphore::new(Duration::from_secs(30), Duration::from_secs(15)))]
mod semaforo_fmu {
    // ─────────────────────────────────────────────────────────────────────────
    // A partir de aquí: el modelo DEVS de Dani, TAL CUAL (solo se quita el
    // `#![no_std]` de crate, que no aplica dentro de un módulo).
    // ─────────────────────────────────────────────────────────────────────────
    use xdevs::{Duration, Port};

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum SemaphoreState {
        Green,
        Red,
    }

    pub struct Semaphore {
        sigma: Duration,
        state: SemaphoreState,
        pressed: bool,
        red_period: Duration,
        green_period: Duration,
    }

    impl xdevs::Component for Semaphore {
        type Input = Port<bool, 1>;
        type Output = Port<SemaphoreState, 1>;
        type Kind = xdevs::AtomicKind;
    }

    impl xdevs::Atomic for Semaphore {
        fn delta_int(&mut self) {
            self.state = match self.state {
                SemaphoreState::Green => {
                    self.sigma = self.red_period;
                    SemaphoreState::Red
                }
                SemaphoreState::Red => {
                    self.sigma = self.green_period;
                    SemaphoreState::Green
                }
            };
        }
        fn lambda(&self, output: &mut Self::Output) {
            let color = match self.state {
                SemaphoreState::Green => SemaphoreState::Red,
                SemaphoreState::Red => SemaphoreState::Green,
            };
            output.add_value(color).unwrap();
        }
        fn delta_ext(&mut self, e: Duration, input: &Self::Input) {
            self.sigma -= e;
            if let Some(&value) = input.get_values().last()
                && value
                && !self.pressed
                && self.state == SemaphoreState::Red
            {
                self.pressed = true;
                self.sigma = self
                    .sigma
                    .checked_sub(Duration::from_secs(10))
                    .unwrap_or(Duration::MIN);
            }
        }
        fn ta(&self) -> Duration {
            self.sigma
        }
    }

    impl Semaphore {
        pub fn new(red_period: Duration, green_period: Duration) -> Self {
            Self {
                sigma: Duration::MIN,
                state: SemaphoreState::Red,
                pressed: false,
                red_period,
                green_period,
            }
        }
    }
}

// ── Smoke test de la FMU que GENERÓ la macro ──────────────────────────────────
// La prueba de comportamiento completa está en el orquestador (config_mod.toml);
// aquí solo se comprueba que el struct generado se construye y despacha por reloj.
#[cfg(test)]
mod tests {
    use super::semaforo_fmu::{SemaphoreFmu, SemaphoreFmuValueRef as Vr};
    use fmi_export::fmi3::UserModel;

    #[test]
    fn la_fmu_generada_se_construye_y_despacha() {
        let s = SemaphoreFmu::default();
        // El reloj countdown `ta` (VR 1) SÍ tiene intervalo; el triggered `input_ev` no.
        assert!(s.next_interval(Vr::Ta.vr()).is_some());
        assert_eq!(s.next_interval(Vr::InputEv.vr()), None);
        // La macro dedujo las variables del enum: green (VR 4) y red (VR 5).
        assert_eq!(Vr::Green as u32, 4);
        assert_eq!(Vr::Red as u32, 5);
    }
}
