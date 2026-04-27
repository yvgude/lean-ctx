use super::super::write_file;
use super::shared::prepare_project_rules_path;

pub(crate) fn install_windsurf_rules(global: bool) {
    let Some(rules_path) = prepare_project_rules_path(global, ".windsurfrules") else {
        return;
    };

    let rules = include_str!("../../templates/windsurfrules.txt");
    write_file(&rules_path, rules);
    println!("Installed .windsurfrules in current project.");
}
