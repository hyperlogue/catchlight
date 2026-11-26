use catchlight_extension_expressions::{
    EMOTIONS, Target, VISEME_CONTROLS, VISEMES, standard_visemes,
};
use catchlight_extension_humanoid::control;

#[test]
fn the_standard_visemes_set_all_seven_controls_within_their_ranges() {
    let visemes = standard_visemes();
    assert_eq!(visemes.len(), 21);
    assert_eq!(
        visemes.iter().map(|v| v.name.as_str()).collect::<Vec<_>>(),
        VISEMES
    );
    for viseme in &visemes {
        let controls: Vec<&str> = viseme
            .settings
            .iter()
            .map(|s| match &s.target {
                Target::Control(name) => name.as_str(),
                Target::Param(_) => panic!("{} sets a param", viseme.name),
            })
            .collect();
        assert_eq!(controls, VISEME_CONTROLS, "{}", viseme.name);
        for setting in &viseme.settings {
            let Target::Control(name) = &setting.target else {
                unreachable!()
            };
            let c = control(name).unwrap_or_else(|| panic!("{name} is not standard"));
            assert!(
                (c.min()..=c.max()).contains(&setting.value),
                "{} sets {name} to {}",
                viseme.name,
                setting.value
            );
        }
    }
}

#[test]
fn the_standard_expression_and_viseme_names_are_distinct() {
    let mut names: Vec<&str> = VISEMES.iter().chain(&EMOTIONS).copied().collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), 26);
}
