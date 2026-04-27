macro_rules! static_regex {
    ($pattern:expr) => {{
        static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        RE.get_or_init(|| {
            regex::Regex::new($pattern).expect(concat!("BUG: invalid static regex: ", $pattern))
        })
    }};
}

fn class_re() -> &'static regex::Regex {
    static_regex!(
        r"(?:abstract\s+)?class\s+(\w+)(?:\s+extends\s+(\w+))?(?:\s+implements\s+([\w,\s\\]+))?"
    )
}
fn method_re() -> &'static regex::Regex {
    static_regex!(
        r"(?:public|protected|private)\s+(?:static\s+)?function\s+(\w+)\s*\(([^)]*)\)(?:\s*:\s*(\S+))?"
    )
}
fn use_re() -> &'static regex::Regex {
    static_regex!(r"^use\s+([\w\\]+)(?:\s+as\s+(\w+))?\s*;")
}
fn extends_re() -> &'static regex::Regex {
    static_regex!(r"extends\s+([\w\\]+)")
}
fn relation_re() -> &'static regex::Regex {
    static_regex!(
        r"\$this->(hasMany|hasOne|belongsTo|belongsToMany|morphMany|morphTo|morphOne|hasManyThrough|hasOneThrough)\s*\(\s*(\w+)::class"
    )
}
fn fillable_re() -> &'static regex::Regex {
    static_regex!(r"\$fillable\s*=\s*\[([\s\S]*?)\]")
}
fn casts_re() -> &'static regex::Regex {
    static_regex!(r"\$casts\s*=\s*\[([\s\S]*?)\]")
}
fn scope_re() -> &'static regex::Regex {
    static_regex!(r"public\s+function\s+(scope\w+)\s*\(")
}
fn migration_col_re() -> &'static regex::Regex {
    static_regex!(r"\$table->(\w+)\s*\(\s*'(\w+)'(?:\s*,\s*(\d+))?\s*\)")
}
fn migration_table_re() -> &'static regex::Regex {
    static_regex!(r"Schema::create\s*\(\s*'(\w+)'")
}
fn blade_directive_re() -> &'static regex::Regex {
    static_regex!(
        r"@(extends|section|yield|component|include|foreach|if|auth|guest|can|slot|push|stack|livewire|props)\s*\(([^)]*)\)"
    )
}

pub fn compress_php_map(content: &str, filename: &str) -> Option<String> {
    if filename.ends_with(".blade.php") {
        return Some(compress_blade(content));
    }

    let parent = detect_laravel_type(content);
    match parent.as_deref() {
        Some("Model") => Some(compress_eloquent(content)),
        Some("Controller") => Some(compress_controller(content)),
        Some("Migration") if content.contains("Schema::") => Some(compress_migration(content)),
        Some(
            kind @ ("Job" | "Event" | "Listener" | "Notification" | "Mail" | "Policy" | "Request"),
        ) => Some(compress_service_class(content, kind)),
        _ => None,
    }
}

fn detect_laravel_type(content: &str) -> Option<String> {
    if let Some(caps) = extends_re().captures(content) {
        let parent = caps[1].rsplit('\\').next().unwrap_or(&caps[1]);
        return match parent {
            "Model" | "Authenticatable" | "Pivot" => Some("Model".to_string()),
            "Controller" => Some("Controller".to_string()),
            "Migration" => Some("Migration".to_string()),
            "Job" | "ShouldQueue" => Some("Job".to_string()),
            "Event" => Some("Event".to_string()),
            "Listener" | "ShouldHandleEventsAfterCommit" => Some("Listener".to_string()),
            "Notification" => Some("Notification".to_string()),
            "Mailable" | "Mail" => Some("Mail".to_string()),
            "Policy" => Some("Policy".to_string()),
            "FormRequest" => Some("Request".to_string()),
            _ => {
                if content.contains("Schema::create") || content.contains("Schema::table") {
                    Some("Migration".to_string())
                } else {
                    Some(parent.to_string())
                }
            }
        };
    }
    if content.contains("Schema::create") || content.contains("Schema::table") {
        return Some("Migration".to_string());
    }
    None
}

fn compress_eloquent(content: &str) -> String {
    let mut parts = Vec::new();

    if let Some(caps) = class_re().captures(content) {
        parts.push(format!("§ {} extends Model", &caps[1]));
    }

    let imports: Vec<String> = use_re()
        .captures_iter(content)
        .map(|c| c[1].rsplit('\\').next().unwrap_or(&c[1]).to_string())
        .collect();
    if !imports.is_empty() {
        parts.push(format!("  deps: {}", imports.join(", ")));
    }

    if let Some(caps) = fillable_re().captures(content) {
        let fields = extract_quoted_strings(&caps[1]);
        if !fields.is_empty() {
            parts.push(format!("  fillable: {}", fields.join(", ")));
        }
    }

    if let Some(caps) = casts_re().captures(content) {
        let casts = extract_cast_pairs(&caps[1]);
        if !casts.is_empty() {
            parts.push(format!("  casts: {}", casts.join(", ")));
        }
    }

    let relations: Vec<String> = relation_re()
        .captures_iter(content)
        .map(|c| format!("{}({})", &c[1], &c[2]))
        .collect();
    if !relations.is_empty() {
        parts.push(format!("  relations: {}", relations.join(", ")));
    }

    let scopes: Vec<String> = scope_re()
        .captures_iter(content)
        .map(|c| c[1].strip_prefix("scope").unwrap_or(&c[1]).to_string())
        .collect();
    if !scopes.is_empty() {
        parts.push(format!("  scopes: {}", scopes.join(", ")));
    }

    let methods: Vec<String> = method_re()
        .captures_iter(content)
        .filter(|c| !c[1].starts_with("scope"))
        .map(|c| {
            let ret = c.get(3).map_or("", |m| m.as_str());
            if ret.is_empty() {
                c[1].to_string()
            } else {
                format!("{}→{}", &c[1], ret)
            }
        })
        .collect();
    if !methods.is_empty() {
        parts.push(format!("  methods: {}", methods.join(", ")));
    }

    parts.join("\n")
}

fn compress_controller(content: &str) -> String {
    let mut parts = Vec::new();

    if let Some(caps) = class_re().captures(content) {
        parts.push(format!("§ {}", &caps[1]));
    }

    let methods: Vec<String> = method_re()
        .captures_iter(content)
        .map(|c| {
            let name = &c[1];
            let params = compact_params(&c[2]);
            let ret = c.get(3).map_or("", |m| m.as_str());
            if ret.is_empty() {
                format!("  λ {name}({params})")
            } else {
                format!("  λ {name}({params})→{ret}")
            }
        })
        .collect();
    parts.extend(methods);

    parts.join("\n")
}

fn compress_migration(content: &str) -> String {
    let mut parts = Vec::new();

    let tables: Vec<String> = migration_table_re()
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .collect();

    for table in &tables {
        parts.push(format!("+{table} table:"));
    }

    let columns: Vec<String> = migration_col_re()
        .captures_iter(content)
        .filter_map(|c| {
            let col_type = &c[1];
            let col_name = &c[2];
            if col_type == "table" || col_type == "create" {
                return None;
            }
            let short_type = shorten_column_type(col_type);
            Some(format!("  {col_name}:{short_type}"))
        })
        .collect();
    parts.extend(columns);

    let has_timestamps = content.contains("$table->timestamps()");
    let has_soft_deletes = content.contains("softDeletes");
    if has_timestamps {
        parts.push("  timestamps".to_string());
    }
    if has_soft_deletes {
        parts.push("  softDeletes".to_string());
    }

    if parts.is_empty() {
        return "migration (empty)".to_string();
    }
    parts.join("\n")
}

fn compress_service_class(content: &str, kind: &str) -> String {
    let mut parts = Vec::new();

    if let Some(caps) = class_re().captures(content) {
        parts.push(format!("§ {} [{}]", &caps[1], kind));
    }

    let constructor: Vec<String> = content
        .lines()
        .filter(|l| {
            let t = l.trim();
            t.contains("public function __construct") || (t.contains("private ") && t.contains('$'))
        })
        .take(1)
        .flat_map(|l| {
            if let Some(caps) = method_re().captures(l) {
                vec![format!("  __construct({})", compact_params(&caps[2]))]
            } else {
                vec![]
            }
        })
        .collect();
    parts.extend(constructor);

    let methods: Vec<String> = method_re()
        .captures_iter(content)
        .filter(|c| &c[1] != "__construct")
        .map(|c| {
            let ret = c.get(3).map_or("", |m| m.as_str());
            if ret.is_empty() {
                format!("  λ {}", &c[1])
            } else {
                format!("  λ {}→{}", &c[1], ret)
            }
        })
        .collect();
    parts.extend(methods);

    parts.join("\n")
}

fn compress_blade(content: &str) -> String {
    let mut parts = Vec::new();

    let directives: Vec<String> = blade_directive_re()
        .captures_iter(content)
        .map(|c| {
            let dir = &c[1];
            let arg = c[2].trim().trim_matches('\'').trim_matches('"');
            format!("@{dir}({arg})")
        })
        .collect();

    if directives.is_empty() {
        let line_count = content.lines().count();
        return format!("blade template ({line_count}L, no directives)");
    }

    let mut seen = std::collections::HashSet::new();
    for d in &directives {
        if seen.insert(d.clone()) {
            parts.push(d.clone());
        }
    }

    parts.join("\n")
}

fn compact_params(params: &str) -> String {
    params
        .split(',')
        .map(|p| {
            let p = p.trim();
            if let Some(var) = p.rsplit_once(' ') {
                var.1.to_string()
            } else {
                p.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn extract_quoted_strings(text: &str) -> Vec<String> {
    static_regex!(r"'(\w+)'")
        .captures_iter(text)
        .map(|c| c[1].to_string())
        .collect()
}

fn extract_cast_pairs(text: &str) -> Vec<String> {
    static_regex!(r"'(\w+)'\s*=>\s*'(\w+)'")
        .captures_iter(text)
        .map(|c| format!("{}:{}", &c[1], &c[2]))
        .collect()
}

fn shorten_column_type(t: &str) -> &str {
    match t {
        "string" => "str",
        "integer" => "int",
        "bigInteger" | "unsignedBigInteger" => "bigint",
        "boolean" => "bool",
        "timestamp" | "timestampTz" => "ts",
        "nullableTimestamps" => "ts?",
        "text" => "text",
        "json" | "jsonb" => "json",
        "decimal" | "float" | "double" => "num",
        "foreignId" | "foreignIdFor" => "fk",
        "uuid" => "uuid",
        "enum" => "enum",
        "date" => "date",
        "dateTime" | "dateTimeTz" => "datetime",
        "id" => "id",
        _ => t,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eloquent_model_compression() {
        let model = r"<?php
namespace App\Models;

use Illuminate\Database\Eloquent\Model;
use Illuminate\Database\Eloquent\Relations\HasMany;

class User extends Model
{
    protected $fillable = ['name', 'email', 'password'];
    protected $casts = ['verified_at' => 'datetime', 'is_admin' => 'boolean'];

    public function posts(): HasMany
    {
        return $this->hasMany(Post::class);
    }

    public function scopeActive($query)
    {
        return $query->where('active', true);
    }

    public function getFullNameAttribute(): string
    {
        return $this->first_name . ' ' . $this->last_name;
    }
}
";
        let result = compress_eloquent(model);
        assert!(result.contains("User extends Model"), "class header");
        assert!(
            result.contains("fillable: name, email, password"),
            "fillable"
        );
        assert!(result.contains("verified_at:datetime"), "casts");
        assert!(result.contains("hasMany(Post)"), "relations");
        assert!(result.contains("scopes: Active"), "scopes");
    }

    #[test]
    fn controller_compression() {
        let ctrl = r"<?php
namespace App\Http\Controllers;

class UserController extends Controller
{
    public function index(): View
    {
        return view('users.index');
    }

    public function store(StoreUserRequest $request): RedirectResponse
    {
        User::create($request->validated());
        return redirect()->route('users.index');
    }

    public function show(User $user): View
    {
        return view('users.show', compact('user'));
    }
}
";
        let result = compress_controller(ctrl);
        assert!(result.contains("UserController"), "class name");
        assert!(result.contains("λ index"), "index method");
        assert!(result.contains("λ store"), "store method");
        assert!(result.contains("λ show"), "show method");
    }

    #[test]
    fn migration_compression() {
        let migration = r"<?php
use Illuminate\Database\Migrations\Migration;
use Illuminate\Database\Schema\Blueprint;

return new class extends Migration
{
    public function up(): void
    {
        Schema::create('users', function (Blueprint $table) {
            $table->id('id');
            $table->string('name');
            $table->string('email', 255);
            $table->timestamp('verified_at');
            $table->boolean('is_admin');
            $table->timestamps();
        });
    }
};
";
        let result = compress_migration(migration);
        assert!(result.contains("+users table:"), "table name");
        assert!(result.contains("name:str"), "string column");
        assert!(result.contains("email:str"), "string with length");
        assert!(result.contains("is_admin:bool"), "boolean");
        assert!(result.contains("timestamps"), "timestamps");
    }

    #[test]
    fn blade_template_compression() {
        let blade = r#"
@extends('layouts.app')

@section('content')
<div class="container mx-auto px-4 py-8">
    <h1 class="text-2xl font-bold mb-4">Users</h1>
    @foreach($users as $user)
        <div class="card">
            @include('partials.user-card')
        </div>
    @endforeach

    @if(auth()->check())
        @component('components.admin-panel')
            Admin content here
        @endcomponent
    @endif
</div>
@endsection
"#;
        let result = compress_blade(blade);
        assert!(result.contains("@extends(layouts.app)"), "extends");
        assert!(result.contains("@section(content)"), "section");
        assert!(result.contains("@foreach"), "foreach");
        assert!(result.contains("@include"), "include");
        assert!(!result.contains("<div"), "no raw HTML");
    }

    #[test]
    fn service_class_compression() {
        let job = r"<?php
namespace App\Jobs;

class SendWelcomeEmail extends Job implements ShouldQueue
{
    public function __construct(
        private User $user,
        private string $template
    ) {}

    public function handle(Mailer $mailer): void
    {
        $mailer->send($this->template, $this->user);
    }

    public function failed(\Throwable $e): void
    {
        Log::error($e->getMessage());
    }
}
";
        let result = compress_service_class(job, "Job");
        assert!(result.contains("SendWelcomeEmail [Job]"), "class + kind");
        assert!(result.contains("λ handle"), "handle method");
        assert!(result.contains("λ failed"), "failed method");
    }

    #[test]
    fn detect_laravel_types() {
        assert_eq!(
            detect_laravel_type("class User extends Model {"),
            Some("Model".to_string())
        );
        assert_eq!(
            detect_laravel_type("class UserController extends Controller {"),
            Some("Controller".to_string())
        );
        assert_eq!(
            detect_laravel_type("Schema::create('users', function"),
            Some("Migration".to_string())
        );
    }
}
