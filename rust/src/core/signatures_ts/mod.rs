mod queries;

mod ast_prune;
mod extract;
mod grammar_loader;
mod handlers;
mod helpers;
mod query_cache;
pub(crate) mod sfc;

pub use ast_prune::ast_prune;
pub use extract::extract_signatures_ts;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rust_signatures() {
        let src = r#"
pub struct Config {
    name: String,
}

pub enum Status {
    Active,
    Inactive,
}

pub trait Handler {
    fn handle(&self);
}

impl Handler for Config {
    fn handle(&self) {
        println!("handling");
    }
}

pub async fn process(input: &str) -> Result<String, Error> {
    Ok(input.to_string())
}

fn helper(x: i32) -> bool {
    x > 0
}
"#;
        let sigs = extract_signatures_ts(src, "rs").unwrap();
        assert!(sigs.len() >= 5, "expected >=5 sigs, got {}", sigs.len());

        let names: Vec<&str> = sigs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Config"));
        assert!(names.contains(&"Status"));
        assert!(names.contains(&"Handler"));
        assert!(names.contains(&"process"));
        assert!(names.contains(&"helper"));
    }

    #[test]
    fn test_typescript_signatures() {
        let src = r"
export function greet(name: string): string {
    return `Hello ${name}`;
}

export class UserService {
    async findUser(id: number): Promise<User> {
        return db.find(id);
    }
}

export interface Config {
    host: string;
    port: number;
}

export type UserId = string;

const handler = async (req: Request): Promise<Response> => {
    return new Response();
};
";
        let sigs = extract_signatures_ts(src, "ts").unwrap();
        assert!(sigs.len() >= 5, "expected >=5 sigs, got {}", sigs.len());

        let names: Vec<&str> = sigs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"greet"));
        assert!(names.contains(&"UserService"));
        assert!(names.contains(&"Config"));
        assert!(names.contains(&"UserId"));
        assert!(names.contains(&"handler"));
    }

    #[test]
    fn test_python_signatures() {
        let src = r"
class AuthService:
    def __init__(self, db):
        self.db = db

    async def authenticate(self, email: str, password: str) -> bool:
        user = await self.db.find(email)
        return check(user, password)

def create_app() -> Flask:
    return Flask(__name__)

def _internal_helper(x):
    return x * 2
";
        let sigs = extract_signatures_ts(src, "py").unwrap();
        assert!(sigs.len() >= 4, "expected >=4 sigs, got {}", sigs.len());

        let names: Vec<&str> = sigs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"AuthService"));
        assert!(names.contains(&"authenticate"));
        assert!(names.contains(&"create_app"));

        let auth = sigs.iter().find(|s| s.name == "authenticate").unwrap();
        assert!(auth.is_async);
        assert_eq!(auth.kind, "method");

        let helper = sigs.iter().find(|s| s.name == "_internal_helper").unwrap();
        assert!(!helper.is_exported);
    }

    #[test]
    fn test_go_signatures() {
        let src = r"
package main

type Config struct {
    Host string
    Port int
}

type Handler interface {
    Handle() error
}

func NewConfig(host string, port int) *Config {
    return &Config{Host: host, Port: port}
}

func (c *Config) Validate() error {
    return nil
}

func helper() {
}
";
        let sigs = extract_signatures_ts(src, "go").unwrap();
        assert!(sigs.len() >= 4, "expected >=4 sigs, got {}", sigs.len());

        let names: Vec<&str> = sigs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Config"));
        assert!(names.contains(&"Handler"));
        assert!(names.contains(&"NewConfig"));
        assert!(names.contains(&"Validate"));

        let nc = sigs.iter().find(|s| s.name == "NewConfig").unwrap();
        assert!(nc.is_exported);

        let h = sigs.iter().find(|s| s.name == "helper").unwrap();
        assert!(!h.is_exported);
    }

    #[test]
    fn test_java_signatures() {
        let src = r"
public class UserController {
    public UserController(UserService service) {
        this.service = service;
    }

    public User getUser(int id) {
        return service.findById(id);
    }

    private void validate(User user) {
        // validation logic
    }
}

public interface Repository {
    User findById(int id);
}

public enum Role {
    ADMIN,
    USER
}
";
        let sigs = extract_signatures_ts(src, "java").unwrap();
        assert!(sigs.len() >= 4, "expected >=4 sigs, got {}", sigs.len());

        let names: Vec<&str> = sigs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"UserController"));
        assert!(names.contains(&"getUser"));
        assert!(names.contains(&"Repository"));
        assert!(names.contains(&"Role"));
    }

    #[test]
    fn test_c_signatures() {
        let src = r"
typedef unsigned int uint;

struct Config {
    char* host;
    int port;
};

enum Status {
    ACTIVE,
    INACTIVE
};

int process(const char* input, int len) {
    return 0;
}

void cleanup(struct Config* cfg) {
    free(cfg);
}
";
        let sigs = extract_signatures_ts(src, "c").unwrap();
        assert!(sigs.len() >= 3, "expected >=3 sigs, got {}", sigs.len());

        let names: Vec<&str> = sigs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"process"));
        assert!(names.contains(&"cleanup"));
    }

    #[test]
    fn test_cpp_signatures() {
        // Regression: a bad `namespace_definition` node name used to make the
        // whole C++ query fail to compile, dropping every `.cpp` file to the
        // regex fallback. `extract_signatures_ts` is the tree-sitter path, so a
        // non-empty result here proves the query compiles and matches.
        let src = r"
namespace net {

class Socket {
public:
    int connect(const char* host, int port);
};

struct Packet {
    int len;
};

enum Mode { Sync, Async };

int send_all(Socket& s, const Packet& p) {
    return 0;
}

}
";
        let sigs = extract_signatures_ts(src, "cpp").unwrap();
        let names: Vec<&str> = sigs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"net"), "namespace, got {names:?}");
        assert!(names.contains(&"Socket"), "class, got {names:?}");
        assert!(names.contains(&"Packet"), "struct, got {names:?}");
        assert!(names.contains(&"Mode"), "enum, got {names:?}");
        assert!(names.contains(&"send_all"), "function, got {names:?}");
    }

    #[test]
    fn test_ruby_signatures() {
        let src = r"
module Authentication
  class UserService
    def initialize(db)
      @db = db
    end

    def authenticate(email, password)
      user = @db.find(email)
      user&.check(password)
    end

    def self.create(config)
      new(config[:db])
    end
  end
end
";
        let sigs = extract_signatures_ts(src, "rb").unwrap();
        assert!(sigs.len() >= 3, "expected >=3 sigs, got {}", sigs.len());

        let names: Vec<&str> = sigs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"UserService"));
        assert!(names.contains(&"authenticate"));
    }

    #[test]
    fn test_multiline_rust_signature() {
        let src = r"
pub fn complex_function<T: Display + Debug>(
    first_arg: &str,
    second_arg: Vec<T>,
    third_arg: Option<HashMap<String, Vec<u8>>>,
) -> Result<(), Box<dyn Error>> {
    Ok(())
}
";
        let sigs = extract_signatures_ts(src, "rs").unwrap();
        assert!(!sigs.is_empty(), "should parse multiline function");
        assert_eq!(sigs[0].name, "complex_function");
        assert!(sigs[0].is_exported);
        assert_eq!(sigs[0].start_line, Some(2));
        assert_eq!(sigs[0].end_line, Some(8));
    }

    #[test]
    fn test_arrow_function_ts() {
        let src = r"
export const fetchData = async (url: string): Promise<Response> => {
    return fetch(url);
};

const internal = (x: number) => x * 2;
";
        let sigs = extract_signatures_ts(src, "ts").unwrap();
        assert!(sigs.len() >= 2, "expected >=2 sigs, got {}", sigs.len());

        let fetch = sigs.iter().find(|s| s.name == "fetchData").unwrap();
        assert!(fetch.is_async);
        assert!(fetch.is_exported);
        assert_eq!(fetch.kind, "fn");
        assert_eq!(fetch.start_line, Some(2));
        assert_eq!(fetch.end_line, Some(4));

        let internal = sigs.iter().find(|s| s.name == "internal").unwrap();
        assert!(!internal.is_exported);
        assert_eq!(internal.start_line, Some(6));
        assert_eq!(internal.end_line, Some(6));
    }

    #[test]
    fn test_csharp_signatures() {
        let src = r"
namespace Demo;
public record Person(string Name);
public interface IRepo { void Save(); }
public struct Point { public int X; }
public enum Role { Admin, User }
public class Service {
    public string Hello(string name) => name;
}
";
        let sigs = extract_signatures_ts(src, "cs").unwrap();
        let names: Vec<&str> = sigs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Person"), "got {names:?}");
        assert!(names.contains(&"IRepo"));
        assert!(names.contains(&"Point"));
        assert!(names.contains(&"Role"));
        assert!(names.contains(&"Service"));
        assert!(names.contains(&"Hello"));
    }

    #[test]
    fn test_kotlin_signatures() {
        let src = r#"
class UserService {
    fun greet(name: String): String = "Hi $name"
}
object Factory {
    fun build(): UserService = UserService()
}
interface Handler {
    fun handle()
}
"#;
        let sigs = extract_signatures_ts(src, "kt").unwrap();
        let names: Vec<&str> = sigs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"UserService"), "got {names:?}");
        assert!(names.contains(&"Factory"));
        assert!(names.contains(&"Handler"));
        assert!(names.contains(&"greet"));
        assert!(names.contains(&"build"));
        assert!(names.contains(&"handle"));
    }

    #[test]
    fn test_kotlin_signature_spans() {
        let src = r#"
class Service {
    suspend fun release(id: String): Boolean =
        repository.release(id)

    fun block_body(name: String): String {
        return "ok $name"
    }
}
"#;
        let sigs = extract_signatures_ts(src, "kt").unwrap();

        let release = sigs.iter().find(|s| s.name == "release").unwrap();
        assert_eq!(release.start_line, Some(3));
        assert_eq!(release.end_line, Some(4));

        let block_body = sigs.iter().find(|s| s.name == "block_body").unwrap();
        assert_eq!(block_body.start_line, Some(6));
        assert_eq!(block_body.end_line, Some(8));
    }

    #[test]
    fn test_swift_signatures() {
        let src = r"
class Box {
    func size() -> Int { 0 }
}
struct Point {
    var x: Int
}
enum Kind { case a, b }
protocol Drawable {
    func draw()
}
func topLevel() {}
";
        let sigs = extract_signatures_ts(src, "swift").unwrap();
        let names: Vec<&str> = sigs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Box"), "got {names:?}");
        assert!(names.contains(&"Point"));
        assert!(names.contains(&"Kind"));
        assert!(names.contains(&"Drawable"));
        assert!(names.contains(&"size"));
        assert!(names.contains(&"draw"));
        assert!(names.contains(&"topLevel"));
    }

    #[test]
    fn test_php_signatures() {
        let src = r"<?php
function helper(int $x): int { return $x; }
class User {
    public function name(): string { return ''; }
}
interface IAuth { public function check(): bool; }
trait Loggable { function log(): void {} }
";
        let sigs = extract_signatures_ts(src, "php").unwrap();
        let names: Vec<&str> = sigs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"helper"), "got {names:?}");
        assert!(names.contains(&"User"));
        assert!(names.contains(&"name"));
        assert!(names.contains(&"IAuth"));
        assert!(names.contains(&"check"));
        assert!(names.contains(&"Loggable"));
        assert!(names.contains(&"log"));
    }

    #[test]
    fn test_unsupported_extension_returns_none() {
        let sigs = extract_signatures_ts("some content", "xyz");
        assert!(sigs.is_none());
    }

    #[test]
    fn test_bash_signatures() {
        let src = r#"
greet() {
    echo "Hello $1"
}

function cleanup {
    rm -rf /tmp/build
}

function deploy() {
    echo "deploying"
}
"#;
        let sigs = extract_signatures_ts(src, "sh").unwrap();
        assert!(sigs.len() >= 2, "expected >=2 sigs, got {}", sigs.len());
        let names: Vec<&str> = sigs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"greet"), "got {names:?}");
        assert!(names.contains(&"cleanup"), "got {names:?}");
    }

    #[test]
    fn test_dart_signatures() {
        let src = r"
class UserService {
  Future<User> getUser(int id) async {
    return db.find(id);
  }
}

enum Status { active, inactive }

mixin Logging {
  void log(String msg) => print(msg);
}

typedef JsonMap = Map<String, dynamic>;
";
        let sigs = extract_signatures_ts(src, "dart").unwrap();
        let names: Vec<&str> = sigs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"UserService"), "got {names:?}");
        assert!(names.contains(&"Status"), "got {names:?}");
        assert!(names.contains(&"Logging"), "got {names:?}");
    }

    #[test]
    fn test_scala_signatures() {
        let src = r"
package example

trait Handler {
  def handle(): Unit
}

class UserService(db: Database) {
  def findUser(id: Int): Option[User] = db.find(id)
  private def validate(user: User): Boolean = true
}

object Factory {
  def create(): UserService = new UserService(db)
}

enum Color:
  case Red, Green, Blue

type UserId = String
";
        let sigs = extract_signatures_ts(src, "scala").unwrap();
        let names: Vec<&str> = sigs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Handler"), "got {names:?}");
        assert!(names.contains(&"UserService"), "got {names:?}");
        assert!(names.contains(&"Factory"), "got {names:?}");
        assert!(names.contains(&"findUser"), "got {names:?}");
    }

    #[test]
    fn test_elixir_signatures() {
        let src = r"
defmodule MyApp.UserService do
  def get_user(id) do
    Repo.get(User, id)
  end

  defp validate(user) do
    user.valid?
  end

  defmacro trace(expr) do
    quote do: IO.inspect(unquote(expr))
  end
end

defprotocol Printable do
  def print(data)
end
";
        let sigs = extract_signatures_ts(src, "ex").unwrap();
        let names: Vec<&str> = sigs.iter().map(|s| s.name.as_str()).collect();
        assert!(
            names.contains(&"MyApp.UserService") || names.contains(&"UserService"),
            "got {names:?}"
        );
    }

    #[test]
    fn test_svelte_signatures() {
        let src = r#"
<script lang="ts">
export function greet(name: string): string {
    return `Hello ${name}`;
}

export class Counter {
    count = 0;
    increment() { this.count++; }
}
</script>

<div>{greeting}</div>
"#;
        let sigs = extract_signatures_ts(src, "svelte").unwrap();
        let names: Vec<&str> = sigs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"greet"), "got {names:?}");
        assert!(names.contains(&"Counter"), "got {names:?}");

        // Spans must be shifted back from the <script> block to file lines.
        let greet = sigs.iter().find(|s| s.name == "greet").unwrap();
        assert_eq!(greet.start_line, Some(3));
        assert_eq!(greet.end_line, Some(5));
    }

    #[test]
    fn test_vue_signatures() {
        let src = r"
<template>
  <div>{{ msg }}</div>
</template>

<script>
export default {
  name: 'MyComponent'
}

export function helper(x) {
    return x * 2;
}

export class DataService {
    fetch() { return []; }
}
</script>
";
        let sigs = extract_signatures_ts(src, "vue").unwrap();
        let names: Vec<&str> = sigs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"helper"), "got {names:?}");
        assert!(names.contains(&"DataService"), "got {names:?}");
    }

    #[test]
    fn test_zig_signatures() {
        let src = r#"
const std = @import("std");

pub fn init(allocator: std.mem.Allocator) !*Self {
    return allocator.create(Self);
}

fn helper(x: u32) u32 {
    return x * 2;
}

pub fn main() !void {
    std.debug.print("hello\n", .{});
}
"#;
        let sigs = extract_signatures_ts(src, "zig").unwrap();
        let names: Vec<&str> = sigs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"init"), "got {names:?}");
        assert!(names.contains(&"helper"), "got {names:?}");
        assert!(names.contains(&"main"), "got {names:?}");

        let init_sig = sigs.iter().find(|s| s.name == "init").unwrap();
        assert!(init_sig.is_exported);
        let helper_sig = sigs.iter().find(|s| s.name == "helper").unwrap();
        assert!(!helper_sig.is_exported);
    }

    #[test]
    fn test_gdscript_signatures() {
        let src = r#"
class_name Player
extends "res://actors/base_actor.gd"

signal health_changed(old, new)

enum State { IDLE, RUNNING }

func _ready() -> void:
    pass

func take_damage(amount: int) -> int:
    return amount

class Inventory:
    func add(item) -> void:
        pass
"#;
        let sigs = extract_signatures_ts(src, "gd").unwrap();
        let names: Vec<&str> = sigs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Player"), "got {names:?}");
        assert!(names.contains(&"health_changed"), "got {names:?}");
        assert!(names.contains(&"State"), "got {names:?}");
        assert!(names.contains(&"take_damage"), "got {names:?}");
        assert!(names.contains(&"Inventory"), "got {names:?}");

        let player = sigs.iter().find(|s| s.name == "Player").unwrap();
        assert_eq!(player.kind, "class");
        assert!(player.is_exported);

        let signal = sigs.iter().find(|s| s.name == "health_changed").unwrap();
        assert_eq!(signal.kind, "signal");

        // `_ready` is a Godot virtual callback → private by `_` convention.
        let ready = sigs.iter().find(|s| s.name == "_ready").unwrap();
        assert!(!ready.is_exported);
        assert_eq!(ready.return_type, "void");

        let take_damage = sigs.iter().find(|s| s.name == "take_damage").unwrap();
        assert!(take_damage.is_exported);
        assert_eq!(take_damage.kind, "fn");
    }

    #[test]
    fn test_gdscript_member_signatures() {
        // #316: `@export`/`@onready`/`const`/`var` members must surface as symbols,
        // while function-local `var`s must NOT (they are not part of the API).
        let src = r"
extends Node

const MAX_HEALTH = 100
var health = 100
var _internal_state = 0
@export var speed: float = 5.0
@onready var sprite = $Sprite

func _process(delta):
    var local_tmp = delta * 2
    return local_tmp
";
        let sigs = extract_signatures_ts(src, "gd").unwrap();
        let names: Vec<&str> = sigs.iter().map(|s| s.name.as_str()).collect();

        assert!(names.contains(&"MAX_HEALTH"), "got {names:?}");
        assert!(names.contains(&"health"), "got {names:?}");
        assert!(names.contains(&"speed"), "got {names:?}");
        assert!(names.contains(&"sprite"), "got {names:?}");
        assert!(
            !names.contains(&"local_tmp"),
            "function-local var must not be a member symbol; got {names:?}"
        );

        let max_health = sigs.iter().find(|s| s.name == "MAX_HEALTH").unwrap();
        assert_eq!(max_health.kind, "const");
        assert!(max_health.is_exported);

        let speed = sigs.iter().find(|s| s.name == "speed").unwrap();
        assert_eq!(speed.kind, "var");
        assert!(speed.is_exported, "@export var is public API");

        // Leading-underscore plain member follows the Godot privacy convention.
        let internal = sigs.iter().find(|s| s.name == "_internal_state").unwrap();
        assert!(!internal.is_exported);
    }

    #[test]
    fn test_lua_signatures() {
        let src = r"
local function helper(a, b)
    return a + b
end

function PublicApi(x)
    return x
end

function Account.new(balance)
    return setmetatable({ balance = balance }, Account)
end

function Account:deposit(amount)
    self.balance = self.balance + amount
end

Account.reset = function()
end
";
        let sigs = extract_signatures_ts(src, "lua").unwrap();
        let names: Vec<&str> = sigs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"helper"), "got {names:?}");
        assert!(names.contains(&"PublicApi"), "got {names:?}");
        assert!(names.contains(&"new"), "table function, got {names:?}");
        assert!(names.contains(&"deposit"), "method, got {names:?}");
        assert!(names.contains(&"reset"), "assigned function, got {names:?}");

        // `local function` is module-private; everything else is public.
        let helper = sigs.iter().find(|s| s.name == "helper").unwrap();
        assert_eq!(helper.kind, "fn");
        assert!(!helper.is_exported, "local function must be private");
        assert_eq!(helper.params, "a, b");

        let public = sigs.iter().find(|s| s.name == "PublicApi").unwrap();
        assert!(public.is_exported, "global function is public");

        let deposit = sigs.iter().find(|s| s.name == "deposit").unwrap();
        assert_eq!(deposit.kind, "method", "`:` defines a method");
        assert!(deposit.is_exported);

        let new = sigs.iter().find(|s| s.name == "new").unwrap();
        assert_eq!(new.kind, "fn", "`.` defines a plain function");
    }

    #[test]
    fn test_luau_signatures() {
        let src = r"
local function helper(a: number): number
    return a
end

export type Vec = { x: number, y: number }
type Internal = { id: string }

function M.run(self, count: number): ()
end
";
        let sigs = extract_signatures_ts(src, "luau").unwrap();
        let names: Vec<&str> = sigs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"helper"), "got {names:?}");
        assert!(names.contains(&"Vec"), "exported type, got {names:?}");
        assert!(names.contains(&"Internal"), "local type, got {names:?}");
        assert!(names.contains(&"run"), "table function, got {names:?}");

        let vec = sigs.iter().find(|s| s.name == "Vec").unwrap();
        assert_eq!(vec.kind, "type");
        assert!(vec.is_exported, "`export type` is public");

        let internal = sigs.iter().find(|s| s.name == "Internal").unwrap();
        assert!(!internal.is_exported, "plain `type` is module-local");

        let helper = sigs.iter().find(|s| s.name == "helper").unwrap();
        assert!(!helper.is_exported);
        assert_eq!(helper.return_type, "number", "Luau return type captured");
    }

    /// Every extension the signature engine advertises must resolve to a
    /// grammar *and* a query that compiles into a usable cache entry. This is
    /// the coverage guard for the language count claimed in tool descriptions
    /// and docs: if a new grammar/query is wired up incorrectly (bad node name,
    /// missing arm), the ext drops out of the cache and this test fails.
    #[test]
    fn every_supported_ext_has_a_compiling_query() {
        use tree_sitter::Query;
        // One representative extension per distinct language (canonical alias).
        let langs: &[&str] = &[
            "rs", "ts", "js", "py", "go", "java", "c", "cpp", "rb", "cs", "kt", "swift", "php",
            "sh", "dart", "scala", "ex", "zig", "gd", "lua", "luau", "ml", "mli", "hs", "jl",
            "sol", "nix", "ps1",
        ];
        let mut failures = Vec::new();
        for ext in langs {
            match (
                super::queries::get_language(ext),
                super::queries::get_query(ext),
            ) {
                (Some(lang), Some(src)) => {
                    if let Err(e) = Query::new(&lang, src) {
                        failures.push(format!(".{ext}: {e:?}"));
                    }
                }
                _ => failures.push(format!(".{ext}: no language/query mapping")),
            }
        }
        assert!(
            failures.is_empty(),
            "signature queries failed to compile:\n{}",
            failures.join("\n")
        );
    }

    #[test]
    fn test_ocaml_signatures() {
        let src = r#"
let add x y = x + y

let pi = 3.14

type color = Red | Green | Blue

module Auth = struct
  let token = "secret"
end

module type Service = sig
  val run : unit -> unit
end

external identity : 'a -> 'a = "%identity"
"#;
        let sigs = extract_signatures_ts(src, "ml").unwrap();
        let names: Vec<&str> = sigs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"add"), "got {names:?}");
        assert!(names.contains(&"color"), "got {names:?}");
        assert!(names.contains(&"Auth"), "got {names:?}");
        assert!(names.contains(&"Service"), "got {names:?}");
        assert!(names.contains(&"identity"), "got {names:?}");

        let auth = sigs.iter().find(|s| s.name == "Auth").unwrap();
        assert_eq!(auth.kind, "module");
        let color = sigs.iter().find(|s| s.name == "color").unwrap();
        assert_eq!(color.kind, "type");
    }

    #[test]
    fn test_ocaml_interface_signatures() {
        let src = r"
val connect : string -> int -> unit

type connection

module Pool : sig
  val acquire : unit -> connection
end
";
        let sigs = extract_signatures_ts(src, "mli").unwrap();
        let names: Vec<&str> = sigs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"connect"), "got {names:?}");
        assert!(names.contains(&"connection"), "got {names:?}");
        assert!(names.contains(&"Pool"), "got {names:?}");
    }

    #[test]
    fn test_haskell_signatures() {
        let src = r#"
module Demo where

add :: Int -> Int -> Int
add x y = x + y

main :: IO ()
main = putStrLn "hello"

data Color = Red | Green | Blue

newtype Wrapper = Wrapper Int

type Name = String

class Greet a where
  greet :: a -> String
"#;
        let sigs = extract_signatures_ts(src, "hs").unwrap();
        let names: Vec<&str> = sigs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"add"), "got {names:?}");
        assert!(names.contains(&"main"), "got {names:?}");
        assert!(names.contains(&"Color"), "got {names:?}");
        assert!(names.contains(&"Wrapper"), "got {names:?}");
        assert!(names.contains(&"Name"), "got {names:?}");
        assert!(names.contains(&"Greet"), "got {names:?}");
    }

    #[test]
    fn test_julia_signatures() {
        let src = r"
function add(x, y)
    x + y
end

square(x) = x * x

struct Point
    x::Int
    y::Int
end

abstract type Shape end

module Geometry
end

macro sayhello(name)
    return :(println($name))
end
";
        let sigs = extract_signatures_ts(src, "jl").unwrap();
        let names: Vec<&str> = sigs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"add"), "got {names:?}");
        assert!(names.contains(&"square"), "got {names:?}");
        assert!(names.contains(&"Point"), "got {names:?}");
        assert!(names.contains(&"Shape"), "got {names:?}");
        assert!(names.contains(&"Geometry"), "got {names:?}");

        let point = sigs.iter().find(|s| s.name == "Point").unwrap();
        assert_eq!(point.kind, "struct");
    }

    #[test]
    fn test_solidity_signatures() {
        let src = r"
contract Token {
    event Transfer(address indexed to, uint value);

    struct Account {
        uint balance;
    }

    enum Status { Active, Frozen }

    modifier onlyOwner() {
        _;
    }

    function transfer(address to, uint amount) public returns (bool) {
        return true;
    }
}

interface IERC20 {
    function totalSupply() external view returns (uint);
}

library SafeMath {
    function add(uint a, uint b) internal pure returns (uint) {
        return a + b;
    }
}
";
        let sigs = extract_signatures_ts(src, "sol").unwrap();
        let names: Vec<&str> = sigs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Token"), "got {names:?}");
        assert!(names.contains(&"transfer"), "got {names:?}");
        assert!(names.contains(&"onlyOwner"), "got {names:?}");
        assert!(names.contains(&"Transfer"), "got {names:?}");
        assert!(names.contains(&"Account"), "got {names:?}");
        assert!(names.contains(&"Status"), "got {names:?}");
        assert!(names.contains(&"IERC20"), "got {names:?}");
        assert!(names.contains(&"SafeMath"), "got {names:?}");

        let token = sigs.iter().find(|s| s.name == "Token").unwrap();
        assert_eq!(token.kind, "class");
    }

    #[test]
    fn test_nix_signatures() {
        let src = r"
{
  mkService = name: port: {
    inherit name port;
  };

  greet = who: ''Hello ${who}'';

  plainValue = 42;
}
";
        let sigs = extract_signatures_ts(src, "nix").unwrap();
        let names: Vec<&str> = sigs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"mkService"), "got {names:?}");
        assert!(names.contains(&"greet"), "got {names:?}");
        // `plainValue` is a non-function binding → not a navigable symbol.
        assert!(
            !names.contains(&"plainValue"),
            "non-function binding must be skipped; got {names:?}"
        );
    }

    #[test]
    fn test_powershell_signatures() {
        let src = r"
function Get-CargoBinDir {
    param([string]$Path)
    return $Path
}

function Stop-RunningLeanCtx() {
    Write-Host 'stopping'
}

class BuildResult {
    [string]$Name
    [void] Publish($target) {
    }
}

enum BuildKind {
    Debug
    Release
}
";
        let sigs = extract_signatures_ts(src, "ps1").unwrap();
        let names: Vec<&str> = sigs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Get-CargoBinDir"), "got {names:?}");
        assert!(names.contains(&"Stop-RunningLeanCtx"), "got {names:?}");
        assert!(names.contains(&"BuildResult"), "got {names:?}");
        assert!(names.contains(&"BuildKind"), "got {names:?}");
        assert!(names.contains(&"Publish"), "got {names:?}");

        let cls = sigs.iter().find(|s| s.name == "BuildResult").unwrap();
        assert_eq!(cls.kind, "class");
        let f = sigs.iter().find(|s| s.name == "Get-CargoBinDir").unwrap();
        assert_eq!(f.kind, "fn");
    }
}
