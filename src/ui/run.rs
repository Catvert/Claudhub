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
/// recipe a bare `just` runs, then the others, in the order `just` lists
/// them.
///
/// The default joins even when it is private: a justfile often opens on a
/// `[private] default` menu, which `just --list` hides and a bare `just`
/// runs. Left out, the widget fell back on the first recipe of the
/// alphabet.
pub fn configs(env: bool, default: Option<&str>, recipes: &[String]) -> Vec<RunConfig> {
    env.then_some(RunConfig::Env)
        .into_iter()
        .chain(default.map(|name| RunConfig::Recipe(name.to_string())))
        .chain(
            recipes
                .iter()
                .filter(|name| Some(name.as_str()) != default)
                .cloned()
                .map(RunConfig::Recipe),
        )
        .collect()
}

/// The configuration on show: the one chosen, as long as the worktree
/// still has it; else the first there is — the environment, else the
/// recipe a bare `just` runs (`configs` puts them there).
pub fn shown(chosen: Option<&RunConfig>, configs: &[RunConfig]) -> Option<RunConfig> {
    chosen
        .filter(|chosen| configs.contains(chosen))
        .or_else(|| configs.first())
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recipes(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    fn recipe(name: &str) -> RunConfig {
        RunConfig::Recipe(name.into())
    }

    #[test]
    fn the_environment_comes_before_the_recipes() {
        assert_eq!(
            configs(true, None, &recipes(&["run", "test"])),
            vec![RunConfig::Env, recipe("run"), recipe("test")]
        );
        assert_eq!(configs(false, None, &[]), vec![]);
    }

    /// The default leads the recipes, once, and is there even when the
    /// justfile hides it from `just --list`.
    #[test]
    fn the_default_recipe_leads_private_or_not() {
        assert_eq!(
            configs(true, Some("run"), &recipes(&["build", "run", "test"])),
            vec![
                RunConfig::Env,
                recipe("run"),
                recipe("build"),
                recipe("test")
            ]
        );
        assert_eq!(
            configs(false, Some("default"), &recipes(&["adminer-prod", "build"])),
            vec![recipe("default"), recipe("adminer-prod"), recipe("build")]
        );
    }

    #[test]
    fn the_one_chosen_is_shown_while_it_exists() {
        let all = configs(true, Some("run"), &recipes(&["run", "test"]));
        let test = recipe("test");
        assert_eq!(shown(Some(&test), &all), Some(test));
        // A recipe gone from the justfile: back to the environment.
        assert_eq!(shown(Some(&recipe("deploy")), &all), Some(RunConfig::Env));
        // No environment: the recipe a bare `just` runs, private or not.
        let plain = configs(false, Some("default"), &recipes(&["build", "run"]));
        assert_eq!(shown(None, &plain), Some(recipe("default")));
        let bare = configs(false, None, &recipes(&["build", "run"]));
        assert_eq!(shown(None, &bare), Some(recipe("build")));
        assert_eq!(shown(None, &[]), None);
    }
}
