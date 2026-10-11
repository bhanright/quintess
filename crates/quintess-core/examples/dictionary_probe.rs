//! Loads every bundled dictionary and checks a sentence in each, timing the load.
fn main() {
    let samples = [
        ("es-ES", "El contrato se firma mañana por la tarde", "contarto"),
        ("fr-FR", "Le contrat sera signé demain après-midi", "contrta"),
        ("de-DE", "Der Vertrag wird morgen Nachmittag unterschrieben", "Vertarg"),
        ("pt-BR", "O contrato será assinado amanhã à tarde", "contarto"),
        ("it-IT", "Il contratto sarà firmato domani pomeriggio", "contartto"),
    ];
    for (language, correct, wrong) in samples {
        let started = std::time::Instant::now();
        let checker = quintess_core::spelling::SpellChecker::new(Some(language)).expect(language);
        let loaded = started.elapsed();
        let found: Vec<String> = checker.check(correct).into_iter().map(|m| m.word).collect();
        let suggestions = checker.suggestions(wrong);
        println!("{language}: loaded in {loaded:?}; flagged in a correct sentence: {found:?}; {wrong} -> {:?}", &suggestions[..suggestions.len().min(3)]);
    }
}
