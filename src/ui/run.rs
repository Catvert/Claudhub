//! What the run widget offers, as an IDE's does: the configurations a
//! worktree has — its environment, which `wt up` starts and `wt down` stops,
//! and each recipe of its `justfile` — and which of them is the one on show.
//! Pure; the widget is `run_view`.

/// One thing the widget can start and stop.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum RunConfig {
    /// The project's environment, as `wt.toml` declares it.
    Env,
    /// A recipe of the `justfile`, by name.
    Recipe(String),
}

/// A worktree's configurations: its environment first when its project
/// declares one — it is what the others usually need running — then the
/// recipes, in the order `just` lists them.
pub fn configs(env: bool, recipes: &[String]) -> Vec<RunConfig> {
    env.then_some(RunConfig::Env)
        .into_iter()
        .chain(recipes.iter().cloned().map(RunConfig::Recipe))
        .collect()
}

/// The configuration on show: the one chosen, as long as the worktree
/// still has it; else the environment; else the recipe a bare `just` runs;
/// else the first there is.
pub fn shown(
    chosen: Option<&RunConfig>,
    configs: &[RunConfig],
    default_recipe: Option<&str>,
) -> Option<RunConfig> {
    if let Some(chosen) = chosen.filter(|chosen| configs.contains(chosen)) {
        return Some(chosen.clone());
    }
    if configs.contains(&RunConfig::Env) {
        return Some(RunConfig::Env);
    }
    default_recipe
        .map(|name| RunConfig::Recipe(name.to_string()))
        .filter(|config| configs.contains(config))
        .or_else(|| configs.first().cloned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recipes(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn the_environment_comes_before_the_recipes() {
        assert_eq!(
            configs(true, &recipes(&["run", "test"])),
            vec![
                RunConfig::Env,
                RunConfig::Recipe("run".into()),
                RunConfig::Recipe("test".into())
            ]
        );
        assert_eq!(configs(false, &[]), vec![]);
    }

    #[test]
    fn the_one_chosen_is_shown_while_it_exists() {
        let all = configs(true, &recipes(&["run", "test"]));
        let test = RunConfig::Recipe("test".into());
        assert_eq!(shown(Some(&test), &all, Some("run")), Some(test));
        // A recipe gone from the justfile: back to the environment.
        let gone = RunConfig::Recipe("deploy".into());
        assert_eq!(shown(Some(&gone), &all, Some("run")), Some(RunConfig::Env));
        // No environment: the recipe a bare `just` runs.
        let plain = configs(false, &recipes(&["build", "run"]));
        assert_eq!(
            shown(None, &plain, Some("run")),
            Some(RunConfig::Recipe("run".into()))
        );
        assert_eq!(
            shown(None, &plain, None),
            Some(RunConfig::Recipe("build".into()))
        );
        assert_eq!(shown(None, &[], None), None);
    }
}
