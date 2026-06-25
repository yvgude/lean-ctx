package main

import (
	"encoding/json"
	"fmt"
	"log"
	"net/http"
	"os"
	"time"
)

type App struct {
	port    string
	started time.Time
}

func NewApp(port string) *App {
	return &App{
		port:    port,
		started: time.Now(),
	}
}

func (a *App) healthHandler(w http.ResponseWriter, r *http.Request) {
	w.Header().Set("Content-Type", "application/json")
	json.NewEncoder(w).Encode(map[string]interface{}{
		"status":  "ok",
		"uptime":  time.Since(a.started).String(),
		"version": "1.0.0",
	})
}

func (a *App) rootHandler(w http.ResponseWriter, r *http.Request) {
	fmt.Fprintf(w, "Hello from Go app on port %s", a.port)
}

func main() {
	port := os.Getenv("PORT")
	if port == "" {
		port = "8080"
	}

	app := NewApp(port)
	mux := http.NewServeMux()
	mux.HandleFunc("/health", app.healthHandler)
	mux.HandleFunc("/", app.rootHandler)

	log.Printf("Starting server on :%s", port)
	log.Fatal(http.ListenAndServe(":"+port, mux))
}
