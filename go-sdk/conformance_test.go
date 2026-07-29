package ocla

import (
	"context"
	"encoding/json"
	"os"
	"testing"
)

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
	envelope := conformanceEnvelope()

	t.Run("health", func(t *testing.T) {
		h, err := client.Health()
		if err != nil {
			t.Fatal(err)
		}
		if len(h.Overall) == 0 {
			t.Fatal("health response has no overall status")
		}
	})
	t.Run("health_version", func(t *testing.T) {
		h, err := client.Health()
		if err != nil {
			t.Fatal(err)
		}
		if h.Version != OclaAPIVersion {
			t.Fatalf("version = %q, want %q", h.Version, OclaAPIVersion)
		}
	})
	t.Run("capabilities", func(t *testing.T) {
		c, err := client.Capabilities()
		if err != nil {
			t.Fatal(err)
		}
		if len(c.Capabilities) == 0 {
			t.Fatal("no capabilities")
		}
	})
	t.Run("capabilities_version", func(t *testing.T) {
		c, err := client.Capabilities()
		if err != nil {
			t.Fatal(err)
		}
		if c.Version != OclaAPIVersion {
			t.Fatalf("version = %q, want %q", c.Version, OclaAPIVersion)
		}
	})
	t.Run("capability_shape", func(t *testing.T) {
		c, err := client.Capabilities()
		if err != nil {
			t.Fatal(err)
		}
		for _, capability := range c.Capabilities {
			if capability.Kind == "" || capability.APIVersion == "" || capability.Status == "" {
				t.Fatalf("invalid capability: %#v", capability)
			}
		}
	})
	t.Run("validate_envelope", func(t *testing.T) {
		response, err := client.ValidateEnvelope(envelope)
		if err != nil {
			t.Fatal(err)
		}
		if response.SchemaVersion != envelope.SchemaVersion || response.Context.RequestID != envelope.Context.RequestID {
			t.Fatalf("envelope response = %#v", response)
		}
	})
	t.Run("envelope_token_balance", func(t *testing.T) {
		response, err := client.ValidateEnvelope(envelope)
		if err != nil {
			t.Fatal(err)
		}
		if response.TokenBalance != envelope.TokenBalance {
			t.Fatalf("token balance = %#v, want %#v", response.TokenBalance, envelope.TokenBalance)
		}
	})
	t.Run("validate_envelope_batch", func(t *testing.T) {
		encoded, err := json.Marshal(envelope)
		if err != nil {
			t.Fatal(err)
		}
		response, err := client.ValidateEnvelopeBatch([]json.RawMessage{encoded})
		if err != nil {
			t.Fatal(err)
		}
		if len(response) != 1 || !response[0].Valid || response[0].Envelope == nil {
			t.Fatalf("batch response = %#v", response)
		}
	})
	t.Run("agents", func(t *testing.T) {
		response, err := client.Agents()
		if err != nil {
			t.Fatal(err)
		}
		if response.Agents == nil {
			t.Fatal("agents is null")
		}
	})
	t.Run("metrics", func(t *testing.T) {
		response, err := client.Metrics()
		if err != nil {
			t.Fatal(err)
		}
		if response.TraitAdoptionCount == 0 {
			t.Fatal("metrics omitted trait adoption count")
		}
	})
	t.Run("ledger", func(t *testing.T) {
		response, err := client.LedgerSummary()
		if err != nil {
			t.Fatal(err)
		}
		if response.Events > 0 && response.Tokens == 0 {
			t.Fatalf("ledger events = %d with no tokens", response.Events)
		}
	})

	var capsuleRef string
	t.Run("capsule_register", func(t *testing.T) {
		var err error
		capsuleRef, err = client.RegisterCapsule(context.Background(), "go-sdk conformance capsule")
		if err != nil {
			t.Fatal(err)
		}
		if capsuleRef == "" {
			t.Fatal("empty capsule ref")
		}
	})
	t.Run("capsule_resolve", func(t *testing.T) {
		response, err := client.ResolveCapsule(context.Background(), capsuleRef)
		if err != nil {
			t.Fatal(err)
		}
		if response.CapsuleRef != capsuleRef || response.Data != "go-sdk conformance capsule" {
			t.Fatalf("capsule response = %#v", response)
		}
	})
	t.Run("capsule_fork", func(t *testing.T) {
		forked, err := client.ForkCapsule(context.Background(), capsuleRef, 256)
		if err != nil {
			t.Fatal(err)
		}
		if forked == "" || forked == capsuleRef {
			t.Fatalf("forked capsule ref = %q", forked)
		}
	})
}

func conformanceEnvelope() EnvelopeRequest {
	routeRef := "go-sdk-conformance"
	return EnvelopeRequest{
		SchemaVersion: 1,
		Context:       OclaRequestContext{RequestID: "conf-1", SessionID: "conf-s", AgentID: "conf-a", ContentRef: "blake3:test"},
		Surface:       "proxy", Direction: "input", Provider: "openai", Model: "gpt-5",
		TokenBalance: TokenBalance{OriginalTokens: 150, MaterializedTokens: 150, DeliveredTokens: 150, ProviderBilledTokens: 150},
		RouteRef:     &routeRef, IdempotencyKey: "conf-1:input",
	}
}
