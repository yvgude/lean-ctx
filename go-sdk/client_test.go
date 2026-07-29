package ocla

import (
	"context"
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"reflect"
	"strings"
	"testing"
)

var testEnvelope = EnvelopeRequest{
	SchemaVersion: 1,
	Context: OclaRequestContext{
		RequestID: "request-1", SessionID: "session-1", AgentID: "agent-1",
		ContentRef: "blake3:content",
	},
	Surface: "proxy", Direction: "input", Provider: "openai", Model: "gpt-5",
	TokenBalance: TokenBalance{
		OriginalTokens: 100, MaterializedTokens: 80, DeliveredTokens: 60,
		ProviderBilledTokens: 60,
	},
	RouteRef: stringPointer("route-1"), IdempotencyKey: "request-1:input",
}

func stringPointer(value string) *string { return &value }

func intPointer(value int) *int { return &value }

func float64Pointer(value float64) *float64 { return &value }

func TestEnvelopePayloadJSONMarshaling(t *testing.T) {
	tests := []struct {
		name    string
		payload EnvelopePayload
		keys    []string
	}{
		{
			name: "messages",
			payload: EnvelopePayload{
				Type: "messages",
				Messages: []MessageV1{{
					Role:    RoleUser,
					Content: json.RawMessage(`"hello"`),
				}},
			},
			keys: []string{"type", "messages"},
		},
		{
			name: "stream chunk",
			payload: EnvelopePayload{
				Type: "stream_chunk", ChunkIndex: intPointer(2), Delta: "world", FinishReason: "stop",
			},
			keys: []string{"type", "chunk_index", "delta", "finish_reason"},
		},
		{
			name: "tool call",
			payload: EnvelopePayload{
				Type: "tool_call", ToolName: "ctx_read", Arguments: `{"path":"go-sdk/types.go"}`, Result: "ok",
			},
			keys: []string{"type", "tool_name", "arguments", "result"},
		},
		{
			name: "usage",
			payload: EnvelopePayload{
				Type: "usage", InputCostUSD: float64Pointer(0.01), OutputCostUSD: float64Pointer(0.02),
				TotalCostUSD: float64Pointer(0.03), Currency: "USD",
			},
			keys: []string{"type", "input_cost_usd", "output_cost_usd", "total_cost_usd", "currency"},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, err := json.Marshal(EnvelopeRequest{Payload: &tt.payload})
			if err != nil {
				t.Fatal(err)
			}
			var envelope map[string]json.RawMessage
			if err := json.Unmarshal(got, &envelope); err != nil {
				t.Fatal(err)
			}
			var payload EnvelopePayload
			if err := json.Unmarshal(envelope["payload"], &payload); err != nil {
				t.Fatal(err)
			}
			if !reflect.DeepEqual(payload, tt.payload) {
				t.Fatalf("payload = %#v, want %#v", payload, tt.payload)
			}
			var fields map[string]json.RawMessage
			if err := json.Unmarshal(envelope["payload"], &fields); err != nil {
				t.Fatal(err)
			}
			if len(fields) != len(tt.keys) {
				t.Fatalf("payload fields = %#v, want keys %#v", fields, tt.keys)
			}
			for _, key := range tt.keys {
				if _, ok := fields[key]; !ok {
					t.Fatalf("payload fields = %#v, missing %q", fields, key)
				}
			}
		})
	}
}

func TestParseSSEStreamJoinsMultilineData(t *testing.T) {
	events := ParseSSEStream(strings.NewReader("event: envelope\ndata: first\ndata: second\n\nevent: ignored\ndata: third\n\n"))
	got := make([]StreamEvent, 0, 2)
	for event := range events {
		got = append(got, event)
	}
	want := []StreamEvent{
		{Type: "envelope", Data: json.RawMessage("first\nsecond")},
		{Type: "ignored", Data: json.RawMessage("third")},
	}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("events = %#v, want %#v", got, want)
	}
}

func TestStreamEnvelopes(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodGet {
			t.Errorf("method = %q", r.Method)
		}
		if r.URL.Path != "/v1/events" {
			t.Errorf("path = %q", r.URL.Path)
		}
		if got := r.Header.Get("Accept"); got != "text/event-stream" {
			t.Errorf("accept = %q", got)
		}
		if got := r.Header.Get("Authorization"); got != "Bearer stream-key" {
			t.Errorf("authorization = %q", got)
		}
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = io.WriteString(w, "event: envelope\ndata: {\"schema_version\":1,\"context\":{\"request_id\":\"request-1\"},\"payload\":{\"type\":\"stream_chunk\",\"chunk_index\":0,\"delta\":\"hello\"}}\n\n")
		_, _ = io.WriteString(w, "event: heartbeat\ndata: {}\n\n")
	}))
	defer server.Close()

	envelopes, errs := NewClient(server.URL, WithAPIKey("stream-key")).StreamEnvelopes(context.Background())
	var got []EnvelopeRequest
	for env := range envelopes {
		got = append(got, env)
	}
	for err := range errs {
		if err != nil {
			t.Fatal(err)
		}
	}
	if len(got) != 1 {
		t.Fatalf("streamed envelopes = %#v", got)
	}
	if got[0].Payload == nil || got[0].Payload.Type != "stream_chunk" || got[0].Payload.Delta != "hello" {
		t.Fatalf("payload = %#v", got[0].Payload)
	}
}

func TestClientCallsEveryEndpoint(t *testing.T) {
	requests := make([]string, 0, 7)
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		requests = append(requests, r.Method+" "+r.URL.Path)
		w.Header().Set("Content-Type", "application/json")
		switch r.URL.Path {
		case "/ocla/v1/health":
			_, _ = io.WriteString(w, `{"overall":"healthy","components":[],"uptime_seconds":4,"version":"ocla/v1"}`)
		case "/ocla/v1/capabilities":
			_, _ = io.WriteString(w, `{"version":"ocla/v1","capabilities":[]}`)
		case "/ocla/v1/envelope":
			if r.Method != http.MethodPost {
				t.Errorf("envelope method = %s", r.Method)
			}
			_ = json.NewEncoder(w).Encode(testEnvelopeResponse())
		case "/ocla/v1/envelope/batch":
			if r.Method != http.MethodPost {
				t.Errorf("batch method = %s", r.Method)
			}
			var envelopes []json.RawMessage
			if err := json.NewDecoder(r.Body).Decode(&envelopes); err != nil {
				t.Errorf("decode batch body: %v", err)
			}
			if len(envelopes) != 1 || string(envelopes[0]) != `{"schema_version":1}` {
				t.Errorf("batch body = %s", envelopes)
			}
			_ = json.NewEncoder(w).Encode([]BatchEnvelopeResult{{Valid: true}})
		case "/ocla/v1/agents":
			_, _ = io.WriteString(w, `{"agents":[]}`)
		case "/ocla/v1/metrics":
			_, _ = io.WriteString(w, `{"total_events":2,"saved_tokens":40,"saved_usd":0.01,"trait_adoption_count":14}`)
		case "/ocla/v1/ledger/summary":
			_, _ = io.WriteString(w, `{"events":2,"tokens":40,"usd":0.01}`)
		default:
			http.NotFound(w, r)
		}
	}))
	defer server.Close()

	client := NewClient(server.URL)
	if _, err := client.Health(); err != nil {
		t.Fatal(err)
	}
	if _, err := client.Capabilities(); err != nil {
		t.Fatal(err)
	}
	if response, err := client.ValidateEnvelope(testEnvelope); err != nil || response.Provider != "openai" {
		t.Fatalf("envelope = %#v, err = %v", response, err)
	}
	batch := []json.RawMessage{json.RawMessage(`{"schema_version":1}`)}
	if response, err := client.ValidateEnvelopeBatch(batch); err != nil || len(response) != 1 || !response[0].Valid {
		t.Fatalf("batch = %#v, err = %v", response, err)
	}
	if response, err := client.Agents(); err != nil || response.Agents == nil {
		t.Fatalf("agents = %#v, err = %v", response, err)
	}
	if response, err := client.Metrics(); err != nil || response.SavedTokens != 40 {
		t.Fatalf("metrics = %#v, err = %v", response, err)
	}
	if response, err := client.LedgerSummary(); err != nil || response.Tokens != 40 {
		t.Fatalf("ledger = %#v, err = %v", response, err)
	}

	want := []string{
		"GET /ocla/v1/health", "GET /ocla/v1/capabilities",
		"POST /ocla/v1/envelope", "POST /ocla/v1/envelope/batch",
		"GET /ocla/v1/agents", "GET /ocla/v1/metrics",
		"GET /ocla/v1/ledger/summary",
	}
	if !reflect.DeepEqual(requests, want) {
		t.Fatalf("requests = %#v, want %#v", requests, want)
	}
}

func TestClientCallsCapsuleEndpoints(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		switch r.URL.Path {
		case "/ocla/v1/capsule":
			if r.Method != http.MethodPost {
				t.Errorf("register method = %s", r.Method)
			}
			body, err := io.ReadAll(r.Body)
			if err != nil {
				t.Fatal(err)
			}
			if string(body) != "capsule data" {
				t.Errorf("register body = %q", body)
			}
			if got := r.Header.Get("Content-Type"); got != "text/plain" {
				t.Errorf("register content type = %q", got)
			}
			_, _ = io.WriteString(w, `{"capsule_ref":"capsule:1"}`)
		case "/ocla/v1/capsule/capsule:1":
			if r.Method != http.MethodGet {
				t.Errorf("resolve method = %s", r.Method)
			}
			_ = json.NewEncoder(w).Encode(CapsuleData{
				CapsuleRef: "capsule:1", Data: "capsule data",
			})
		case "/ocla/v1/capsule/capsule:1/fork":
			if r.Method != http.MethodPost {
				t.Errorf("fork method = %s", r.Method)
			}
			var payload map[string]int64
			if err := json.NewDecoder(r.Body).Decode(&payload); err != nil {
				t.Fatal(err)
			}
			if payload["budget_tokens"] != 1000 {
				t.Errorf("fork payload = %#v", payload)
			}
			_, _ = io.WriteString(w, `{"capsule_ref":"capsule:2"}`)
		default:
			http.NotFound(w, r)
		}
	}))
	defer server.Close()

	client := NewClient(server.URL)
	registered, err := client.RegisterCapsule(context.Background(), "capsule data")
	if err != nil || registered != "capsule:1" {
		t.Fatalf("registered = %q, err = %v", registered, err)
	}
	resolved, err := client.ResolveCapsule(context.Background(), registered)
	if err != nil || resolved.CapsuleRef != registered || resolved.Data != "capsule data" {
		t.Fatalf("resolved = %#v, err = %v", resolved, err)
	}
	forked, err := client.ForkCapsule(context.Background(), registered, 1000)
	if err != nil || forked != "capsule:2" {
		t.Fatalf("forked = %q, err = %v", forked, err)
	}
}

func TestClientCallsHTTPMCPConformanceEndpoints(t *testing.T) {
	requests := make([]string, 0, 11)
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		requests = append(requests, r.Method+" "+r.URL.RequestURI())
		switch r.URL.Path {
		case "/health":
			w.Header().Set("Content-Type", "text/plain")
			_, _ = io.WriteString(w, "ok")
		case "/v1/events":
			w.Header().Set("Content-Type", "text/event-stream")
		case "/v1/tools":
			if got := r.URL.Query().Get("limit"); got != "1" {
				t.Errorf("tools limit = %q", got)
			}
			w.Header().Set("Content-Type", "application/json")
			_, _ = io.WriteString(w, `{"tools":[{}],"total":1}`)
		case "/v1/tools/call":
			w.Header().Set("Content-Type", "application/json")
			w.WriteHeader(http.StatusBadRequest)
			_, _ = io.WriteString(w, `{"error":"unknown tool","code":"unknown_tool"}`)
		case "/v1/manifest", "/v1/capabilities", "/v1/openapi.json", "/v1/context/summary", "/v1/events/search", "/v1/events/lineage", "/v1/metrics":
			w.Header().Set("Content-Type", "application/json")
			_, _ = io.WriteString(w, `{}`)
		default:
			http.NotFound(w, r)
		}
	}))
	defer server.Close()

	client := NewClient(server.URL, WithAPIKey("secret"))
	if response, err := client.ServerHealth(); err != nil || response != "ok" {
		t.Fatalf("health = %q, err = %v", response, err)
	}
	if _, err := client.Manifest(); err != nil {
		t.Fatal(err)
	}
	if _, err := client.HTTPCapabilities(); err != nil {
		t.Fatal(err)
	}
	if _, err := client.OpenAPI(); err != nil {
		t.Fatal(err)
	}
	if response, err := client.ListTools(1); err != nil || response.Total != 1 {
		t.Fatalf("tools = %#v, err = %v", response, err)
	}
	if _, err := client.CallToolResult("missing"); err == nil {
		t.Fatal("missing tool unexpectedly succeeded")
	}
	if contentType, err := client.EventsProbe(context.Background()); err != nil || !strings.HasPrefix(contentType, "text/event-stream") {
		t.Fatalf("events content type = %q, err = %v", contentType, err)
	}
	if _, err := client.ContextSummary(1); err != nil {
		t.Fatal(err)
	}
	if _, err := client.SearchEvents("probe", 1); err != nil {
		t.Fatal(err)
	}
	if _, err := client.EventLineage(1, 1); err != nil {
		t.Fatal(err)
	}
	if _, err := client.ServerMetrics(); err != nil {
		t.Fatal(err)
	}

	want := []string{
		"GET /health",
		"GET /v1/manifest",
		"GET /v1/capabilities",
		"GET /v1/openapi.json",
		"GET /v1/tools?limit=1",
		"POST /v1/tools/call",
		"GET /v1/events",
		"GET /v1/context/summary?limit=1",
		"GET /v1/events/search?limit=1&q=probe",
		"GET /v1/events/lineage?depth=1&id=1",
		"GET /v1/metrics",
	}
	if !reflect.DeepEqual(requests, want) {
		t.Fatalf("requests = %#v, want %#v", requests, want)
	}
}

func TestClientSendsJSONAndBearerAPIKey(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if got := r.Header.Get("Authorization"); got != "Bearer secret" {
			t.Errorf("authorization = %q", got)
		}
		if got := r.Header.Get("Content-Type"); got != "application/json" {
			t.Errorf("content type = %q", got)
		}
		body, err := io.ReadAll(r.Body)
		if err != nil {
			t.Fatal(err)
		}
		var got EnvelopeRequest
		if err := json.Unmarshal(body, &got); err != nil {
			t.Fatal(err)
		}
		if !reflect.DeepEqual(got, testEnvelope) {
			t.Fatalf("body = %#v, want %#v", got, testEnvelope)
		}
		w.Header().Set("Content-Type", "application/json")
		_ = json.NewEncoder(w).Encode(testEnvelopeResponse())
	}))
	defer server.Close()

	if _, err := NewClient(server.URL, WithAPIKey("secret")).ValidateEnvelope(testEnvelope); err != nil {
		t.Fatal(err)
	}
}

func TestClientOptionsConfigureTransport(t *testing.T) {
	custom := &http.Client{}
	client := NewClient(" https://example.test/// ", WithHTTPClient(custom), WithTimeout(3))
	if client.baseURL != "https://example.test" {
		t.Fatalf("baseURL = %q", client.baseURL)
	}
	if client.httpClient != custom || client.httpClient.Timeout != 3 {
		t.Fatalf("options did not configure client: %#v", client)
	}
}

func TestClientReturnsTypedAPIError(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusBadRequest)
		_, _ = io.WriteString(w, `{"error":"invalid envelope","code":"invalid_request"}`)
	}))
	defer server.Close()

	_, err := NewClient(server.URL).Health()
	var apiError *APIError
	if !strings.Contains(err.Error(), "invalid envelope") || !reflect.TypeOf(err).AssignableTo(reflect.TypeOf(apiError)) {
		t.Fatalf("error = %T %v", err, err)
	}
	if !strings.Contains(err.Error(), "400") {
		t.Fatalf("error = %v", err)
	}
}

func testEnvelopeResponse() EnvelopeResponse {
	return EnvelopeResponse{
		SchemaVersion: testEnvelope.SchemaVersion, Context: testEnvelope.Context,
		Surface: testEnvelope.Surface, Direction: testEnvelope.Direction,
		Provider: testEnvelope.Provider, Model: testEnvelope.Model,
		TokenBalance: testEnvelope.TokenBalance, RouteRef: testEnvelope.RouteRef,
		PolicyRef: testEnvelope.PolicyRef, IdempotencyKey: testEnvelope.IdempotencyKey,
	}
}
