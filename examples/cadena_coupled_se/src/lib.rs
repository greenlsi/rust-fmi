//! **Demo autocontenida de `#[coupled2fmu]`.**
//!
//! Un modelo DEVS **acoplado** con sus dos componentes definidos aquí mismo (no depende de
//! ningún crate de modelos externo): un `Emisor` que lanza un pulso cada 5 s y un
//! `Contador` que cuenta los pulsos recibidos. La macro lo envuelve como una sola FMU cuya
//! salida `output` es la cuenta acumulada; el acoplamiento Emisor→Contador se resuelve
//! **dentro** de la FMU.

#[xdevs_fmi::coupled2fmu(init = Cadena::build(Emisor::new(), Contador::new()))]
mod cadena_fmu {
    use xdevs::{ComponentsInput, ComponentsOutput, Coupled, Duration, Port};

    // ── Componente 1: emite un pulso cada 5 s ────────────────────────────────
    pub struct Emisor {
        sigma: Duration,
    }
    impl xdevs::Component for Emisor {
        type Input = ();
        type Output = Port<bool, 1>;
        type Kind = xdevs::AtomicKind;
    }
    impl xdevs::Atomic for Emisor {
        fn delta_int(&mut self) {
            self.sigma = Duration::from_secs(5);
        }
        fn lambda(&self, o: &mut Self::Output) {
            let _ = o.add_value(true);
        }
        fn delta_ext(&mut self, _e: Duration, _i: &Self::Input) {}
        fn ta(&self) -> Duration {
            self.sigma
        }
    }
    impl Emisor {
        pub fn new() -> Self {
            Self { sigma: Duration::from_secs(5) }
        }
    }

    // ── Componente 2: cuenta los pulsos y expone la cuenta ────────────────────
    pub struct Contador {
        sigma: Duration,
        n: f64,
    }
    impl xdevs::Component for Contador {
        type Input = Port<bool, 1>;
        type Output = Port<f64, 1>;
        type Kind = xdevs::AtomicKind;
    }
    impl xdevs::Atomic for Contador {
        fn delta_int(&mut self) {
            self.sigma = Duration::MAX; // pasivo tras emitir
        }
        fn lambda(&self, o: &mut Self::Output) {
            let _ = o.add_value(self.n);
        }
        fn delta_ext(&mut self, _e: Duration, i: &Self::Input) {
            if i.get_values().last() == Some(&true) {
                self.n += 1.0;
                self.sigma = Duration::MIN; // emitir YA la nueva cuenta
            }
        }
        fn ta(&self) -> Duration {
            self.sigma
        }
    }
    impl Contador {
        pub fn new() -> Self {
            Self { sigma: Duration::MAX, n: 0.0 }
        }
    }

    // ── El modelo ACOPLADO: Emisor → Contador; salida externa = la cuenta ─────
    #[xdevs::coupled]
    pub struct Cadena {
        emisor: Emisor,
        contador: Contador,
    }
    impl xdevs::Component for Cadena {
        type Input = ();
        type Output = Port<f64, 1>;
        type Kind = xdevs::CoupledKind;
    }
    impl Coupled for Cadena {
        fn ic(from: &ComponentsOutput<Self>, to: &mut ComponentsInput<Self>) {
            from.emisor.couple(&mut to.contador).unwrap();
        }
        fn eoc(from: &ComponentsOutput<Self>, to: &mut Self::Output) {
            from.contador.couple(to).unwrap();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::cadena_fmu::{CadenaFmu, CadenaFmuValueRef as Vr};
    use fmi_export::fmi3::UserModel;

    #[test]
    fn la_fmu_acoplada_autonoma_se_construye() {
        let s = CadenaFmu::default();
        assert!(s.next_interval(Vr::Ta.vr()).is_some());
        assert_eq!(Vr::Output as u32, 2);
    }
}
