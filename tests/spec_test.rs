use ezlo::bundled::BUNDLED_SPECS;
use ezlo::spec::span::{RuleCode, Severity};
use ezlo::spec::{has_errors, parse_and_validate};
use serde_json::json;

const SYNTAX_MD_EXAMPLE: &str = r#"
type container(host = opt.host | env.DOCKER_HOST)
  $ docker ${host:+-H "$host"} ps -a --format '{{.ID}}\t{{.Names}}\t{{.State}}'
  cols id name state
  insert id
  show name state
  preview `docker inspect "$id"`
  cache 2s

type container:running = container where state = running
type container:stopped = container where state in (created, exited, dead)

cmd docker "Container runtime"
  opt -H --host <host> "Daemon socket to connect to" global

  recipe "Remove all stopped containers"
    $ docker rm $(docker ps -aq -f status=exited)

cmd docker stop "Stop running containers"
  arg <container...> container:running

cmd docker rm "Remove containers"
  opt -f --force "Force removal of running containers"
  arg <container...> container:stopped
    when --force: container

cmd docker exec "Run a command in a running container"
  opt -i --interactive
  opt -t --tty
  arg <container> container:running
  arg [command...]

  recipe "Shell into a running container"
    $ docker exec -it <container> <shell>
    arg <shell> = sh | bash | zsh

cmd docker logs "Fetch container logs"
  opt -f --follow
  opt -n --tail <n>
  arg <container> container rank state = running

  recipe "Tail logs"
    $ docker logs -f --tail <n> <container>
    arg <n> = 100
"#;

#[test]
fn golden_syntax_md_example_matches_section_5_ast() {
    let (spec, diags) = parse_and_validate(SYNTAX_MD_EXAMPLE);
    assert!(
        diags.is_empty(),
        "expected zero diagnostics on SYNTAX.md example, got: {diags:#?}"
    );

    let val = serde_json::to_value(&spec).expect("serialize spec to JSON");

    // Verify types match SYNTAX.md §5
    let types = val["types"].as_array().expect("types array");
    assert_eq!(types.len(), 3);
    assert_eq!(
        types[0],
        json!({
            "name": "container",
            "params": [{
                "name": "host",
                "default": [
                    {"kind": "opt", "name": "host"},
                    {"kind": "env", "name": "DOCKER_HOST"}
                ]
            }],
            "generator": {
                "kind": "shell",
                "command": "docker ${host:+-H \"$host\"} ps -a --format '{{.ID}}\\t{{.Names}}\\t{{.State}}'"
            },
            "cols": ["id", "name", "state"],
            "insert": "id",
            "show": ["name", "state"],
            "preview": "docker inspect \"$id\"",
            "cache_ms": 2000
        })
    );
    assert_eq!(
        types[1],
        json!({
            "name": "container:running",
            "refines": {
                "ref": { "name": "container", "bindings": [] },
                "where": [{ "col": "state", "op": "eq", "values": ["running"] }],
                "rank": null
            }
        })
    );
    assert_eq!(
        types[2],
        json!({
            "name": "container:stopped",
            "refines": {
                "ref": { "name": "container", "bindings": [] },
                "where": [{ "col": "state", "op": "in", "values": ["created", "exited", "dead"] }],
                "rank": null
            }
        })
    );

    // Verify `docker rm` cmd matches SYNTAX.md §5
    let cmds = val["cmds"].as_array().expect("cmds array");
    let docker_rm = cmds
        .iter()
        .find(|c| c["path"] == json!(["docker", "rm"]))
        .expect("docker rm cmd");
    assert_eq!(
        *docker_rm,
        json!({
            "path": ["docker", "rm"],
            "desc": "Remove containers",
            "opts": [
                {
                    "flags": ["-f", "--force"],
                    "name": "force",
                    "value": null,
                    "desc": "Force removal of running containers",
                    "global": false,
                    "required": false,
                    "excludes": [],
                    "when": []
                }
            ],
            "args": [
                {
                    "name": "container",
                    "required": true,
                    "variadic": true,
                    "type": {
                        "ref": { "name": "container:stopped", "bindings": [] },
                        "where": null,
                        "rank": null
                    },
                    "suggestions": [],
                    "desc": null,
                    "when": [
                        {
                            "cond": [{ "flag": "--force", "negated": false, "value": null }],
                            "type": {
                                "ref": { "name": "container", "bindings": [] },
                                "where": null,
                                "rank": null
                            }
                        }
                    ]
                }
            ],
            "recipes": []
        })
    );

    // Verify recipes match SYNTAX.md §5
    let recipes = val["recipes"].as_array().expect("recipes array");
    assert_eq!(recipes.len(), 3);
    let exec_recipe = recipes
        .iter()
        .find(|r| r["desc"] == "Shell into a running container")
        .expect("exec recipe");
    assert_eq!(
        *exec_recipe,
        json!({
            "desc": "Shell into a running container",
            "scope": ["docker", "exec"],
            "template": "docker exec -it <container> <shell>",
            "placeholders": ["container", "shell"],
            "args": [{
                "name": "shell",
                "variadic": false,
                "type": null,
                "suggestions": ["sh", "bash", "zsh"],
                "desc": null
            }]
        })
    );
}

#[test]
fn rule_e01_lexical_errors() {
    // Tab indentation
    let (_, diags) = parse_and_validate("cmd foo\n\topt -a");
    assert!(diags.iter().any(|d| d.code == RuleCode::E01Lexical && d.severity == Severity::Error));
    assert_eq!(diags[0].span.start.line, 2);

    // Inconsistent sibling indentation
    let (_, diags) = parse_and_validate("cmd foo\n  opt -a\n   opt -b");
    assert!(diags.iter().any(|d| d.code == RuleCode::E01Lexical));

    // Unterminated string
    let (_, diags) = parse_and_validate("cmd foo \"unterminated");
    assert!(diags.iter().any(|d| d.code == RuleCode::E01Lexical));

    // Unknown escape
    let (_, diags) = parse_and_validate("cmd foo \"bad \\q escape\"");
    assert!(diags.iter().any(|d| d.code == RuleCode::E01Lexical));
}

#[test]
fn rule_e02_disallowed_child_or_repeated_property() {
    // Disallowed child under cmd
    let (_, diags) = parse_and_validate("cmd foo\n  cols a b");
    assert!(diags.iter().any(|d| d.code == RuleCode::E02Structure));

    // Repeated property in type block
    let (_, diags) = parse_and_validate("type item\n  values a b\n  cache 1s\n  cache 2s");
    assert!(diags.iter().any(|d| d.code == RuleCode::E02Structure));
}

#[test]
fn rule_e03_type_generator_constraints() {
    // Neither $ nor values
    let (_, diags) = parse_and_validate("type item\n  cols a");
    assert!(diags.iter().any(|d| d.code == RuleCode::E03TypeForm));

    // Both $ and values
    let (_, diags) = parse_and_validate("type item\n  $ echo hi\n  values a b");
    assert!(diags.iter().any(|d| d.code == RuleCode::E03TypeForm));

    // Refinement with children
    let (_, diags) = parse_and_validate("type base\n  values a b\ntype base:sub = base where value = a\n  cache 1s");
    assert!(diags.iter().any(|d| d.code == RuleCode::E03TypeForm));
}

#[test]
fn rule_e04_duplicates_including_inherited_globals() {
    // Duplicate type
    let (_, diags) = parse_and_validate("type t\n  values a\ntype t\n  values b");
    assert!(diags.iter().any(|d| d.code == RuleCode::E04Duplicate));

    // Duplicate cmd path
    let (_, diags) = parse_and_validate("cmd git status\ncmd git status");
    assert!(diags.iter().any(|d| d.code == RuleCode::E04Duplicate));

    // Duplicate flag inherited from global parent
    let src = "cmd app\n  opt -v --verbose global\ncmd app run\n  opt -v --version";
    let (_, diags) = parse_and_validate(src);
    assert!(diags.iter().any(|d| d.code == RuleCode::E04Duplicate));
}

#[test]
fn rule_e05_unresolved_references() {
    // Unresolved type reference
    let (_, diags) = parse_and_validate("cmd app\n  arg <x> unknown_type");
    assert!(diags.iter().any(|d| d.code == RuleCode::E05Unresolved));

    // Unresolved column name in where
    let (_, diags) = parse_and_validate("type t\n  $ echo 1\n  cols id name\ntype t:sub = t where nope = 1");
    assert!(diags.iter().any(|d| d.code == RuleCode::E05Unresolved));

    // Unresolved param name in binding
    let (_, diags) = parse_and_validate("type t(host = env.HOST)\n  values a\ncmd app\n  arg <x> t(wrong = \"1\")");
    assert!(diags.iter().any(|d| d.code == RuleCode::E05Unresolved));

    // Unresolved excludes flag
    let (_, diags) = parse_and_validate("cmd app\n  opt --json excludes --yaml");
    assert!(diags.iter().any(|d| d.code == RuleCode::E05Unresolved));
}

#[test]
fn rule_e06_reserved_words() {
    let (_, diags) = parse_and_validate("type where\n  values a b");
    assert!(diags.iter().any(|d| d.code == RuleCode::E06ReservedWord));

    let (_, diags) = parse_and_validate("type t\n  $ echo 1\n  cols id global");
    assert!(diags.iter().any(|d| d.code == RuleCode::E06ReservedWord));
}

#[test]
fn rule_e07_positional_arg_ordering() {
    // Variadic not last
    let (_, diags) = parse_and_validate("cmd app\n  arg <files...>\n  arg <dest>");
    assert!(diags.iter().any(|d| d.code == RuleCode::E07PositionalArgs));

    // Required after optional
    let (_, diags) = parse_and_validate("cmd app\n  arg [opt_arg]\n  arg <req_arg>");
    assert!(diags.iter().any(|d| d.code == RuleCode::E07PositionalArgs));
}

#[test]
fn rule_e08_recipe_structure() {
    // Missing $ line
    let (_, diags) = parse_and_validate("recipe \"No template\"\n  arg <x> = 1");
    assert!(diags.iter().any(|d| d.code == RuleCode::E08RecipeStructure));

    // arg naming placeholder absent from template
    let (_, diags) = parse_and_validate("recipe \"Bad arg\"\n  $ echo <foo>\n  arg <bar> = 1");
    assert!(diags.iter().any(|d| d.code == RuleCode::E08RecipeStructure));

    // Nested recipe whose template does not start with cmd path
    let (_, diags) = parse_and_validate("cmd git commit\n  recipe \"Wrong prefix\"\n    $ docker ps");
    assert!(diags.iter().any(|d| d.code == RuleCode::E08RecipeStructure));
}

#[test]
fn rule_e09_recipe_placeholder_cycle() {
    let src = r#"
type ta(p = env.X)
  values 1 2
type tb(q = env.Y)
  values 3 4

recipe "Cyclic placeholders"
  $ deploy <a> <b>
  arg <a> ta(p = arg.b)
  arg <b> tb(q = arg.a)
"#;
    let (_, diags) = parse_and_validate(src);
    assert!(diags.iter().any(|d| d.code == RuleCode::E09RecipeCycle));
}

#[test]
fn rule_e10_env_var_and_param_col_collision() {
    // Column & param share the same name
    let src = "type t(id = env.ID)\n  $ echo 1\n  cols id name";
    let (_, diags) = parse_and_validate(src);
    assert!(diags.iter().any(|d| d.code == RuleCode::E10EnvName));

    // Invalid env var name (contains hyphen)
    let src2 = "type t(bad-param = env.ID)\n  values a b";
    let (_, diags2) = parse_and_validate(src2);
    assert!(diags2.iter().any(|d| d.code == RuleCode::E10EnvName));
}

#[test]
fn warning_rules_w01_w02_w03() {
    // W01: Unreachable source after literal
    let (_, diags) = parse_and_validate("type t(p = \"default\" | env.FALLBACK)\n  values a");
    assert!(diags.iter().any(|d| d.code == RuleCode::W01UnreachableSource && d.severity == Severity::Warning));

    // W02: when on an opt that takes no value
    let (_, diags) = parse_and_validate("type t\n  values a\ncmd app\n  opt --flag\n    when --other: t\n  opt --other");
    assert!(diags.iter().any(|d| d.code == RuleCode::W02ValuelessOptWhen && d.severity == Severity::Warning));

    // W03: refinement name prefix differs from base type
    let (_, diags) = parse_and_validate("type pod\n  values a b\ntype container:running = pod where value = a");
    assert!(diags.iter().any(|d| d.code == RuleCode::W03RefinementPrefix && d.severity == Severity::Warning));
}

#[test]
fn all_bundled_specs_validate_cleanly() {
    assert_eq!(BUNDLED_SPECS.len(), 2);
    for (name, src) in BUNDLED_SPECS {
        let (_, diags) = parse_and_validate(src);
        assert!(
            !has_errors(&diags),
            "bundled spec `{name}` had validation errors: {diags:#?}"
        );
        assert!(
            diags.is_empty(),
            "bundled spec `{name}` had warnings: {diags:#?}"
        );
    }
}

fn named(name: &str, src: &str) -> (String, ezlo::spec::ast::Spec) {
    (name.to_string(), parse_and_validate(src).0)
}

#[test]
fn merge_tiers_extends_roots_within_a_tier_and_overlays_lower_tiers() {
    let user = [
        named("dot/_root.ezlo", "cmd dot \"hub\"\n"),
        named("dot/ssh.ezlo", "cmd dot ssh\n\ncmd dot ssh connect\n  arg <device>\n"),
        named("dot/docker.ezlo", "cmd dot docker\n\ncmd dot ssh\n"),
    ];
    let bundled = [named("bundled:dot.ezlo", "cmd dot\n\ncmd dot other\n")];
    let tiers: [&[(String, ezlo::spec::ast::Spec)]; 2] = [&user, &bundled];

    let mut diags = Vec::new();
    let index = ezlo::spec::SpecIndex::merge_tiers(&tiers, &mut diags);
    let paths: Vec<String> = index.cmds.iter().map(|c| c.path.join(" ")).collect();

    assert_eq!(paths, ["dot", "dot ssh", "dot ssh connect", "dot docker", "dot other"]);
    assert_eq!(diags.len(), 1, "{diags:#?}");
    assert_eq!(diags[0].0, "dot/docker.ezlo");
    assert_eq!(diags[0].1.code, RuleCode::E04Duplicate);
}

#[test]
fn merge_ordered_overlays_later_files_under_earlier_ones() {
    let specs = [named("a.ezlo", "cmd foo\n"), named("b.ezlo", "cmd foo\n\ncmd foo bar\n")];
    let mut diags = Vec::new();
    let index = ezlo::spec::SpecIndex::merge_ordered(&specs, &mut diags);
    let paths: Vec<String> = index.cmds.iter().map(|c| c.path.join(" ")).collect();
    assert_eq!(paths, ["foo", "foo bar"]);
    assert!(diags.is_empty(), "{diags:#?}");
}
