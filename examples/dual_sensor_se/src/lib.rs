//! **Demo de `#[atomic2fmu]` con N=2 entradas y M=2 salidas.**
//!
//! El modelo usa **structs de puertos** (`#[derive(xdevs::Bag)]`), que es como xdevs
//! expresa varios puertos — igual que el modelo que pasó Dani. La macro lee esos structs
//! del módulo y genera **una variable FMI (y su reloj triggered) por cada puerto de
//! entrada**, y **las variables de cada puerto de salida** (one-hot si el puerto lleva un
//! enum). Sin declarar nada a mano.
//!
//! - Entradas: `pulso: Port<bool,1>` y `valor: Port<f64,1>` → `pulso`/`valor` + sus relojes.
//! - Salidas:  `media: Port<f64,1>` y `nivel: Port<Nivel,1>` → `media`, y `nivel_bajo`/
//!   `nivel_alto` (one-hot; prefijado por el puerto porque hay varias salidas).

#[xdevs_fmi::atomic2fmu(init = Sensor::new())]
mod sensor_fmu {
    use xdevs::{Duration, Port};

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Nivel {
        Bajo,
        Alto,
    }

    // ── DOS puertos de entrada ────────────────────────────────────────────────
    #[derive(xdevs::Bag)]
    pub struct Entradas {
        pub pulso: Port<bool, 1>, // un pulso incrementa la cuenta
        pub valor: Port<f64, 1>,  // un valor se acumula
    }

    // ── DOS puertos de salida ─────────────────────────────────────────────────
    #[derive(xdevs::Bag)]
    pub struct Salidas {
        pub media: Port<f64, 1>,  // media acumulada
        pub nivel: Port<Nivel, 1>, // Alto si la media supera el umbral
    }

    pub struct Sensor {
        sigma: Duration,
        periodo: Duration,
        acumulado: f64,
        cuenta: u32,
        umbral: f64,
    }

    impl xdevs::Component for Sensor {
        type Input = Entradas;
        type Output = Salidas;
        type Kind = xdevs::AtomicKind;
    }

    impl xdevs::Atomic for Sensor {
        fn delta_int(&mut self) {
            self.sigma = self.periodo; // emite periódicamente
        }

        fn lambda(&self, output: &mut Self::Output) {
            let media = if self.cuenta > 0 {
                self.acumulado / self.cuenta as f64
            } else {
                0.0
            };
            // Emite en LOS DOS puertos de salida.
            output.media.add_value(media).unwrap();
            let nivel = if media > self.umbral { Nivel::Alto } else { Nivel::Bajo };
            output.nivel.add_value(nivel).unwrap();
        }

        fn delta_ext(&mut self, e: Duration, input: &Self::Input) {
            self.sigma = self.sigma.checked_sub(e).unwrap_or(Duration::MIN);
            // Lee LOS DOS puertos de entrada.
            if let Some(&p) = input.pulso.get_values().last() {
                if p {
                    self.cuenta += 1;
                }
            }
            if let Some(&v) = input.valor.get_values().last() {
                self.acumulado += v;
            }
        }

        fn ta(&self) -> Duration {
            self.sigma
        }
    }

    impl Sensor {
        pub fn new() -> Self {
            Self {
                sigma: Duration::from_secs(5),
                periodo: Duration::from_secs(5),
                acumulado: 0.0,
                cuenta: 0,
                umbral: 10.0,
            }
        }
    }
}

// ── Smoke test de la FMU generada (2 in / 2 out) ──────────────────────────────
#[cfg(test)]
mod tests {
    use super::sensor_fmu::{SensorFmu, SensorFmuValueRef as Vr};
    use fmi_export::fmi3::UserModel;

    #[test]
    fn la_fmu_2in_2out_se_construye_y_despacha() {
        let s = SensorFmu::default();
        // Un reloj countdown + dos relojes triggered (uno por entrada).
        assert!(s.next_interval(Vr::Ta.vr()).is_some());
        assert_eq!(s.next_interval(Vr::PulsoEv.vr()), None);
        assert_eq!(s.next_interval(Vr::ValorEv.vr()), None);
        // Las variables deducidas: entradas pulso/valor y salidas media + nivel one-hot.
        let _ = (Vr::Pulso, Vr::Valor, Vr::Media, Vr::NivelBajo, Vr::NivelAlto);
    }
}
