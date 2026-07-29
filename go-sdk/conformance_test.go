package ocla

import (
	"context"
	"os"
	"sort"
	"strings"
	"testing"
)

const httpMCPContractVersion = float64(1)

var coveredRoutes = map[string]string{
	"GET /health":             "ServerHealth",
	"GET /v1/manifest":        "Manifest",
	"GET /v1/capabilities":    "HTTPCapabilities",
	"GET /v1/openapi.json":    "OpenAPI",
	"GET /v1/tools":           "ListTools",
	"POST /v1/tools/call":     "CallToolResult",
	"GET /v1/events":          "EventsProbe",
	"GET /v1/context/summary": "ContextSummary",
	"GET /v1/events/search":   "SearchEvents",
	"GET /v1/events/lineage":  "EventLineage",
	"GET /v1/metrics":         "ServerMetrics",
}

func TestConformanceLive(t *testing.T) {
	url := os.Getenv("LEANCTX_CONFORMANCE_URL")
	if url == "" {
		t.Skip("LEANCTX_CONFORMANCE_URL not set — run via scripts/sdk-conformance.sh")
	}

	opts := []Option{}
	if token := os.Getenv("LEANCTX_CONFORMANCE_TOKEN"); token != "" {
		opts = append(opts, WithAPIKey(token))
	}
	client := NewClient(url, opts...)

	t.Run("health", func(t *testing.T) {
		response, err := client.ServerHealth()
		if err != nil {
			t.Fatal(err)
		}
		if strings.TrimSpace(response) == "" {
			t.Fatal("health response is empty")
		}
	})
	t.Run("manifest_shape", func(t *testing.T) {
		manifest, err := client.Manifest()
		if err != nil {
			t.Fatal(err)
		}
		if len(manifest) == 0 {
			t.Fatal("manifest response is empty")
		}
	})
	t.Run("capabilities_shape", func(t *testing.T) {
		caps, err := client.HTTPCapabilities()
		if err != nil {
			t.Fatal(err)
		}
		if _, ok := caps["contract_version"].(float64); !ok {
			t.Fatalf("capabilities omitted contract_version: %#v", caps)
		}
		if _, ok := caps["server"].(map[string]any); !ok {
			t.Fatalf("capabilities omitted server: %#v", caps)
		}
		if _, ok := caps["features"].(map[string]any); !ok {
			t.Fatalf("capabilities omitted features: %#v", caps)
		}
	})
	t.Run("contract_status_map", func(t *testing.T) {
		caps, err := client.HTTPCapabilities()
		if err != nil {
			t.Fatal(err)
		}
		status, ok := caps["contract_status"].(map[string]any)
		if !ok || (status["http-mcp"] != "frozen" && status["http-mcp"] != "stable") {
			t.Fatalf("contract_status = %#v", caps["contract_status"])
		}
	})
	t.Run("engine_compat", func(t *testing.T) {
		caps, err := client.HTTPCapabilities()
		if err != nil {
			t.Fatal(err)
		}
		contracts, ok := caps["contracts"].(map[string]any)
		if !ok || contracts["leanctx.contract.http_mcp.contract_version"] != httpMCPContractVersion {
			t.Fatalf("contracts = %#v", caps["contracts"])
		}
	})
	t.Run("openapi_shape", func(t *testing.T) {
		doc, err := client.OpenAPI()
		if err != nil {
			t.Fatal(err)
		}
		version, _ := doc["openapi"].(string)
		if !strings.HasPrefix(version, "3.") {
			t.Fatalf("openapi version = %q", version)
		}
		if _, ok := doc["paths"].(map[string]any); !ok {
			t.Fatalf("openapi omitted paths: %#v", doc)
		}
	})
	t.Run("route_coverage", func(t *testing.T) {
		doc, err := client.OpenAPI()
		if err != nil {
			t.Fatal(err)
		}
		if uncovered := uncoveredRoutes(doc); len(uncovered) != 0 {
			t.Fatalf("uncovered routes: %s", strings.Join(uncovered, ", "))
		}
	})
	t.Run("tools_list", func(t *testing.T) {
		response, err := client.ListTools(1)
		if err != nil {
			t.Fatal(err)
		}
		if len(response.Tools) > 1 {
			t.Fatalf("tools = %#v", response)
		}
	})
	t.Run("tool_call_error_contract", func(t *testing.T) {
		_, err := client.CallToolResult("definitely_not_a_tool_conformance_probe")
		apiErr, ok := err.(*APIError)
		if !ok || apiErr.StatusCode < 400 || apiErr.StatusCode >= 500 || (apiErr.Response.Code == "" && apiErr.Response.ErrorCode == "") {
			t.Fatalf("error = %T %v", err, err)
		}
	})
	t.Run("events_stream", func(t *testing.T) {
		contentType, err := client.EventsProbe(context.Background())
		if err != nil {
			t.Fatal(err)
		}
		if !strings.HasPrefix(contentType, "text/event-stream") {
			t.Fatalf("content-type = %q", contentType)
		}
	})
	t.Run("context_summary_shape", func(t *testing.T) {
		response, err := client.ContextSummary(1)
		if err != nil {
			t.Fatal(err)
		}
		if _, ok := response["workspaceId"].(string); !ok {
			t.Fatalf("context summary = %#v", response)
		}
	})
	t.Run("events_search_shape", func(t *testing.T) {
		response, err := client.SearchEvents("conformance-probe", 1)
		if err != nil {
			t.Fatal(err)
		}
		if _, ok := response["results"].([]any); !ok {
			t.Fatalf("events search = %#v", response)
		}
	})
	t.Run("event_lineage_shape", func(t *testing.T) {
		response, err := client.EventLineage(1, 1)
		if err != nil {
			t.Fatal(err)
		}
		if _, ok := response["chain"].([]any); !ok {
			t.Fatalf("event lineage = %#v", response)
		}
	})
	t.Run("metrics_shape", func(t *testing.T) {
		response, err := client.ServerMetrics()
		if err != nil {
			t.Fatal(err)
		}
		if len(response) == 0 {
			t.Fatal("empty metrics")
		}
	})
}

func uncoveredRoutes(doc map[string]any) []string {
	paths, ok := doc["paths"].(map[string]any)
	if !ok {
		return []string{"<missing paths>"}
	}
	uncovered := make([]string, 0)
	for path, value := range paths {
		ops, ok := value.(map[string]any)
		if !ok {
			continue
		}
		for method := range ops {
			route := strings.ToUpper(method) + " " + path
			if _, ok := coveredRoutes[route]; !ok {
				uncovered = append(uncovered, route)
			}
		}
	}
	sort.Strings(uncovered)
	return uncovered
}
