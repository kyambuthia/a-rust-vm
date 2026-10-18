//! `arvm workspace`: durable owned workspaces and their artifacts.

use std::env;

use super::paths::control_plane_path;

const MAX_ARTIFACT_BYTES: u64 = 64 * 1024 * 1024;

fn parse_artifact_kind(value: &str) -> Result<a_rust_vm::artifacts::ArtifactKind, String> {
    match value {
        "source" => Ok(a_rust_vm::artifacts::ArtifactKind::Source),
        "generated" => Ok(a_rust_vm::artifacts::ArtifactKind::Generated),
        _ => Err(format!(
            "invalid artifact kind '{value}': expected source|generated"
        )),
    }
}

fn check_artifact_byte_len(byte_len: u64) -> Result<(), String> {
    if byte_len > MAX_ARTIFACT_BYTES {
        return Err(format!(
            "artifact file too large: {byte_len} bytes exceeds 64 MiB limit"
        ));
    }
    Ok(())
}

struct ArtifactImportArguments<'a> {
    workspace: &'a str,
    artifact_id: &'a str,
    version_id: &'a str,
    file: &'a str,
    media_type: &'a str,
    parent_version_id: Option<&'a str>,
}

fn parse_artifact_import_arguments(
    arguments: &[String],
) -> Result<ArtifactImportArguments<'_>, String> {
    if arguments.len() < 5 || arguments.len() > 6 {
        return Err(
            "usage: arvm workspace artifact import <workspace> <artifact-id> <version-id> <file> <media-type> [parent-version-id]".to_owned(),
        );
    }
    Ok(ArtifactImportArguments {
        workspace: &arguments[0],
        artifact_id: &arguments[1],
        version_id: &arguments[2],
        file: &arguments[3],
        media_type: &arguments[4],
        parent_version_id: arguments.get(5).map(String::as_str),
    })
}

pub(crate) fn run_workspace_command(arguments: &[String]) {
    use a_rust_vm::control_plane::CapabilityGrant;
    use a_rust_vm::control_plane_store::ControlPlaneStore;
    use a_rust_vm::runtime::ResourceLimits;

    let owner = env::var("A_RVM_OWNER")
        .or_else(|_| env::var("USER"))
        .unwrap_or_else(|_| "local".to_owned());
    let store = ControlPlaneStore::new(control_plane_path(), 64, ResourceLimits::default());
    let mut plane = store.load().unwrap_or_else(|error| {
        eprintln!("cannot load workspace state: {error}");
        std::process::exit(1);
    });
    let command = arguments.first().map(String::as_str);
    if command == Some("artifact") {
        if let Err(error) =
            run_workspace_artifact_command(&arguments[1..], &owner, &store, &mut plane)
        {
            eprintln!("workspace error: {error}");
            std::process::exit(1);
        }
        return;
    }
    let result = match command {
        Some("create") if arguments.len() == 2 => {
            plane.create_workspace(&owner, &arguments[1]).map(|()| {
                store.save(&plane).unwrap_or_else(|error| {
                    eprintln!("cannot save workspace state: {error}");
                    std::process::exit(1);
                });
                println!("created workspace {owner}/{}", arguments[1]);
            })
        }
        Some("list") if arguments.len() == 1 => {
            let workspaces = plane.list(&owner);
            if workspaces.is_empty() {
                println!("no workspaces for {owner}");
            } else {
                for workspace in workspaces {
                    println!(
                        "{}\t{:?}\t{} capabilities\t{} observations",
                        workspace.id,
                        workspace.state,
                        workspace.capabilities.len(),
                        workspace.observations.len()
                    );
                }
            }
            Ok(())
        }
        Some("show") if arguments.len() == 2 => {
            plane.inspect(&owner, &arguments[1]).map(|workspace| {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&workspace)
                        .expect("workspace view is serializable")
                );
            })
        }
        Some("grant") if arguments.len() >= 6 => {
            let grant = CapabilityGrant::new(
                &arguments[2],
                &arguments[3],
                &arguments[4],
                arguments[5..].iter().cloned(),
            );
            grant
                .and_then(|grant| plane.grant_capability(&owner, &arguments[1], grant))
                .map(|()| {
                    store.save(&plane).unwrap_or_else(|error| {
                        eprintln!("cannot save workspace state: {error}");
                        std::process::exit(1);
                    });
                    println!(
                        "granted capability {} to {owner}/{}",
                        arguments[2], arguments[1]
                    );
                })
        }
        Some("revoke") if arguments.len() == 3 => plane
            .revoke_capability(&owner, &arguments[1], &arguments[2])
            .map(|()| {
                store.save(&plane).unwrap_or_else(|error| {
                    eprintln!("cannot save workspace state: {error}");
                    std::process::exit(1);
                });
                println!(
                    "revoked capability {} from {owner}/{}",
                    arguments[2], arguments[1]
                );
            }),
        _ => {
            eprintln!(
                "usage:\n  arvm workspace create <id>\n  arvm workspace list\n  arvm workspace show <id>\n  arvm workspace grant <workspace> <capability> <kind> <resource> <action>...\n  arvm workspace revoke <workspace> <capability>\n  arvm workspace artifact create <workspace> <artifact-id> <name> <source|generated>\n  arvm workspace artifact import <workspace> <artifact-id> <version-id> <file> <media-type> [parent-version-id]\n  arvm workspace artifact list <workspace>\n  arvm workspace artifact history <workspace> <artifact-id>\n\nSet A_RVM_OWNER to select the local principal."
            );
            std::process::exit(2);
        }
    };
    if let Err(error) = result {
        eprintln!("workspace error: {error}");
        std::process::exit(1);
    }
}

fn save_workspace_state(
    store: &a_rust_vm::control_plane_store::ControlPlaneStore,
    plane: &a_rust_vm::control_plane::ControlPlane,
) {
    store.save(plane).unwrap_or_else(|error| {
        eprintln!("cannot save workspace state: {error}");
        std::process::exit(1);
    });
}

fn run_workspace_artifact_command(
    arguments: &[String],
    owner: &str,
    store: &a_rust_vm::control_plane_store::ControlPlaneStore,
    plane: &mut a_rust_vm::control_plane::ControlPlane,
) -> Result<(), String> {
    let command = arguments.first().map(String::as_str);
    match command {
        Some("create") if arguments.len() == 5 => {
            let kind = parse_artifact_kind(&arguments[4])?;
            plane
                .create_artifact(
                    owner,
                    &arguments[1],
                    a_rust_vm::artifacts::NewArtifact::new(
                        &arguments[2],
                        &arguments[1],
                        &arguments[3],
                        kind,
                        owner,
                    ),
                )
                .map(|artifact| {
                    save_workspace_state(store, plane);
                    println!("created artifact {} in {owner}/{}", artifact.id, arguments[1]);
                })
                .map_err(|error| error.to_string())
        }
        Some("import") => {
            let request = parse_artifact_import_arguments(&arguments[1..])?;
            let bytes = std::fs::read(request.file)
                .map_err(|error| format!("cannot read '{}': {error}", request.file))?;
            check_artifact_byte_len(bytes.len() as u64)?;
            let stored = store
                .content_store()
                .put(&bytes)
                .map_err(|error| error.to_string())?;
            let mut version = a_rust_vm::artifacts::NewArtifactVersion::new(
                request.version_id,
                request.artifact_id,
                &stored.reference,
                request.media_type,
                stored.byte_len,
                owner,
            );
            if let Some(parent) = request.parent_version_id {
                version = version.with_parent(parent);
            }
            plane
                .append_artifact_version(owner, request.workspace, version)
                .map(|created| {
                    save_workspace_state(store, plane);
                    println!(
                        "imported version {} for {} ({})",
                        created.id, created.artifact_id, created.content_reference
                    );
                })
                .map_err(|error| error.to_string())
        }
        Some("list") if arguments.len() == 2 => plane
            .list_artifacts(owner, &arguments[1])
            .map(|artifacts| {
                if artifacts.is_empty() {
                    println!("no artifacts in {owner}/{}", arguments[1]);
                } else {
                    for artifact in artifacts {
                        println!(
                            "{}\t{}\t{:?}\t{}",
                            artifact.id,
                            artifact.name,
                            artifact.kind,
                            artifact.current_version_id.as_deref().unwrap_or("-")
                        );
                    }
                }
            })
            .map_err(|error| error.to_string()),
        Some("history") if arguments.len() == 3 => plane
            .artifact_version_history(owner, &arguments[1], &arguments[2])
            .map(|versions| {
                for version in versions {
                    println!(
                        "{}\t{}\t{}\t{}",
                        version.id,
                        version.content_reference,
                        version.media_type,
                        version
                            .parent_version_id
                            .as_deref()
                            .unwrap_or("-")
                    );
                }
            })
            .map_err(|error| error.to_string()),
        _ => Err("usage:\n  arvm workspace artifact create <workspace> <artifact-id> <name> <source|generated>\n  arvm workspace artifact import <workspace> <artifact-id> <version-id> <file> <media-type> [parent-version-id]\n  arvm workspace artifact list <workspace>\n  arvm workspace artifact history <workspace> <artifact-id>".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_artifact_kinds() {
        assert!(matches!(
            parse_artifact_kind("source"),
            Ok(a_rust_vm::artifacts::ArtifactKind::Source)
        ));
        assert!(matches!(
            parse_artifact_kind("generated"),
            Ok(a_rust_vm::artifacts::ArtifactKind::Generated)
        ));
        assert!(parse_artifact_kind("blob").is_err());
    }

    #[test]
    fn enforces_artifact_size_limit() {
        assert!(check_artifact_byte_len(64 * 1024 * 1024).is_ok());
        assert!(check_artifact_byte_len(64 * 1024 * 1024 + 1).is_err());
    }

    #[test]
    fn parses_artifact_import_arguments_with_optional_parent() {
        let arguments = [
            "research".to_owned(),
            "budget".to_owned(),
            "budget-v1".to_owned(),
            "budget.xlsx".to_owned(),
            "application/pdf".to_owned(),
            "budget-v0".to_owned(),
        ];
        let parsed = parse_artifact_import_arguments(&arguments).unwrap();
        assert_eq!(parsed.workspace, "research");
        assert_eq!(parsed.parent_version_id, Some("budget-v0"));
        assert!(parse_artifact_import_arguments(&arguments[..2]).is_err());
    }
}
