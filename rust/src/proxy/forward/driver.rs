// SPDX-License-Identifier: Apache-2.0

//! `ExecutionDriver` stages of the shared proxy forward path.

#[allow(clippy::wildcard_imports)]
use super::*;

#[async_trait::async_trait]
impl<F> crate::core::execution_lifecycle::ExecutionDriver for ProxyDriver<'_, F>
where
    F: FnOnce(serde_json::Value, usize) -> (Vec<u8>, usize, usize) + Send,
{
    type Primitive = ProxyPrimitive;
    type Processed = ProxyProcessed;
    type Output = ProxyDispatchResult;
    type Error = StatusCode;

    async fn apply_security_boundaries(
        &mut self,
        _context: &crate::core::execution_lifecycle::TaskContext,
    ) -> Result<crate::core::execution_lifecycle::StageDisposition, Self::Error> {
        let (parts, body) = self
            .request
            .take()
            .expect("proxy request consumed once")
            .into_parts();
        let body_limit =
            super::super::bedrock::request_body_limit(&parts).unwrap_or_else(max_body_bytes);
        let raw_body_bytes = axum::body::to_bytes(body, body_limit)
            .await
            .map_err(|_| StatusCode::PAYLOAD_TOO_LARGE)?;
        // Enforce the requested model before routing or context transformations.
        #[cfg(feature = "enterprise")]
        let gate_rules = super::super::policy_gate::active_rules().map_err(|error| {
            tracing::error!("gateway org policy rejected: {error}");
            StatusCode::SERVICE_UNAVAILABLE
        })?;
        #[cfg(feature = "enterprise")]
        if let Some(rules) = &gate_rules {
            let tags = parts
                .extensions
                .get::<super::super::gateway_identity::GatewayTags>()
                .cloned()
                .unwrap_or_default();
            let requested_model = prepare::requested_model_of(&parts, &raw_body_bytes);
            if let Err(refusal) =
                super::super::policy_gate::enforce(rules, requested_model.as_deref(), &tags)
            {
                tracing::warn!(
                    "lean-ctx gateway: org policy refused request ({refusal:?}) person={:?} project={:?}",
                    tags.person,
                    tags.project
                );
                self.prepared_request = Some(PreparedProxyRequest::Terminal(Box::new(
                    ProxyPrimitive::Policy {
                        response: super::super::policy_gate::refusal_response(
                            &refusal,
                            self.provider_label,
                        ),
                        original_tokens: raw_body_bytes.len() / 4,
                        trace_id: self.trace_id.clone(),
                    },
                )));
                return Ok(crate::core::execution_lifecycle::StageDisposition::Applied);
            }
        }
        self.admitted = Some(AdmittedProxyRequest {
            parts,
            raw_body_bytes,
            body_limit,
            #[cfg(feature = "enterprise")]
            gate_rules,
        });
        Ok(crate::core::execution_lifecycle::StageDisposition::Applied)
    }

    async fn gather_context_strategy(
        &mut self,
        _context: &crate::core::execution_lifecycle::TaskContext,
    ) -> Result<crate::core::execution_lifecycle::StageDisposition, Self::Error> {
        if self.prepared_request.is_some() {
            return Ok(crate::core::execution_lifecycle::StageDisposition::Skipped(
                "org policy refused request",
            ));
        }
        let extra_stream_types: Vec<&str> =
            self.extra_stream_types.iter().map(String::as_str).collect();
        self.prepared_request = Some(
            prepare_upstream_request(
                State(self.state.take().expect("proxy state consumed once")),
                self.admitted
                    .take()
                    .expect("security admission precedes preparation"),
                self.upstream_base,
                self.default_path,
                self.compress_body
                    .take()
                    .expect("proxy compressor consumed once"),
                self.provider_label,
                &extra_stream_types,
                self.trace_id.clone(),
            )
            .await?,
        );
        Ok(crate::core::execution_lifecycle::StageDisposition::Applied)
    }

    async fn dispatch_primitive(
        &mut self,
        _context: &crate::core::execution_lifecycle::TaskContext,
    ) -> Result<
        (
            Self::Primitive,
            crate::core::execution_lifecycle::StageDisposition,
        ),
        Self::Error,
    > {
        match self
            .prepared_request
            .take()
            .expect("request prepared before dispatch")
        {
            PreparedProxyRequest::Terminal(primitive) => {
                let reason = match primitive.as_ref() {
                    ProxyPrimitive::Policy { .. } => "org policy refused request",
                    ProxyPrimitive::Cache { .. } => "response cache hit",
                    ProxyPrimitive::Upstream { .. } => {
                        unreachable!("upstream result is not a prepared terminal")
                    }
                };
                Ok((
                    *primitive,
                    crate::core::execution_lifecycle::StageDisposition::Skipped(reason),
                ))
            }
            PreparedProxyRequest::Upstream(outbound) => {
                let ProxyOutbound {
                    parts,
                    upstream_url,
                    upstream_base,
                    forwarded_body,
                    preserve_content_encoding,
                    mut prepared,
                } = *outbound;
                // Optional billable probes are dispatch effects too: never start
                // them while request preparation can still reject the request.
                if self.provider_label == "Anthropic"
                    && !prepared.xlat
                    && let Some(wire) = prepared.wire.as_mut()
                {
                    wire.counterfactual = super::super::counterfactual::maybe_spawn_probe(
                        &prepared.state.client,
                        &parts,
                        &upstream_base,
                        prepared.introspect.as_ref().map(|(parsed, _)| parsed),
                        prepared
                            .route
                            .as_ref()
                            .map(|route| route.routed_from.as_str()),
                        prepared.compressed_size < prepared.original_size,
                    );
                }
                prepared.upstream_started = Some(std::time::Instant::now());
                let response = transport::send_upstream(
                    &prepared.state,
                    &parts,
                    &upstream_url,
                    forwarded_body,
                    self.provider_label,
                    preserve_content_encoding,
                )
                .await;
                prepared.upstream_send_succeeded = response.is_ok();
                Ok((
                    ProxyPrimitive::Upstream { response, prepared },
                    crate::core::execution_lifecycle::StageDisposition::Applied,
                ))
            }
        }
    }

    async fn reversible_post_process(
        &mut self,
        _context: &crate::core::execution_lifecycle::TaskContext,
        primitive: Self::Primitive,
    ) -> Result<
        (
            Self::Processed,
            crate::core::execution_lifecycle::StageDisposition,
        ),
        Self::Error,
    > {
        match primitive {
            ProxyPrimitive::Policy {
                response,
                original_tokens,
                trace_id,
            } => Ok((
                ProxyProcessed {
                    result: {
                        let mut response = response;
                        trace_id::inject_trace_id(&mut response, &trace_id);
                        proxy_dispatch_result(response, 0, original_tokens, "policy_denied")
                    },
                    prepared: None,
                    skip_reason: Some("org policy refused request"),
                },
                crate::core::execution_lifecycle::StageDisposition::Skipped(
                    "org policy refused request",
                ),
            )),
            ProxyPrimitive::Cache {
                mut response,
                prepared,
            } => {
                if let Ok(value) =
                    HeaderValue::from_str(&prepared.cache_alignment_score.to_string())
                {
                    response
                        .headers_mut()
                        .insert("x-leanctx-cache-alignment", value);
                }
                super::super::determinism_guard::apply_response_headers(
                    &mut response,
                    &prepared.determinism_proof,
                );
                trace_id::inject_trace_id(&mut response, &prepared.determinism_proof.request_id);
                let result = proxy_dispatch_result(
                    response,
                    prepared.tokens_saved as usize,
                    prepared.original_tokens,
                    "cache_hit",
                );
                Ok((
                    ProxyProcessed {
                        result,
                        prepared: Some(prepared),
                        skip_reason: Some("response cache hit"),
                    },
                    crate::core::execution_lifecycle::StageDisposition::Skipped(
                        "response cache hit",
                    ),
                ))
            }
            ProxyPrimitive::Upstream {
                response,
                mut prepared,
            } => {
                let extra_stream_types: Vec<&str> = prepared
                    .extra_stream_types
                    .iter()
                    .map(String::as_str)
                    .collect();
                let wire = prepared.wire.take();
                let response = match response {
                    Ok(response) => {
                        transport::build_response(
                            response,
                            prepared.upstream_started,
                            &extra_stream_types,
                            prepared.usage_provider,
                            prepared.url_model.clone(),
                            prepared.cohort,
                            wire,
                            prepared.xlat,
                            prepared.state.ocla_cache.as_deref(),
                            prepared.model.as_deref(),
                            &prepared.cache_prompt_hash,
                        )
                        .await
                    }
                    Err(status) => Err(status),
                };
                // #1774: OpenAI answers a ChatGPT-subscription OAuth token on the
                // platform `/v1` rail with `Missing scopes: api.responses.write` —
                // a message about organization roles and API-key scopes that sends
                // people hunting through their OpenAI settings for a permission that
                // was never the problem. The real cause is the rail: a subscription
                // token only authenticates against chatgpt.com.
                //
                // #1685 moved the clear-cut case off this path entirely: a JWT bearer
                // on a stock `api.openai.com` upstream is now re-routed to the ChatGPT
                // rail before it is sent (`openai_responses::chatgpt_rail_uri`). What
                // still reaches here is the residue that cannot be re-routed safely —
                // a configured gateway upstream, or a credential whose shape says
                // nothing — so the annotation stays as the explanation of last resort.
                let response = match response {
                    Ok(response)
                        if self.provider_label == "OpenAI"
                            && response.status() == StatusCode::UNAUTHORIZED =>
                    {
                        Ok(annotate_openai_scope_401(response).await)
                    }
                    other => other,
                };
                let mut response = match response {
                    Ok(response) => response,
                    Err(status) => {
                        // Preserve the attempted request for the ledger even if
                        // sending or decoding failed; return the original error
                        // after lifecycle finalization, without inventing usage.
                        let result = ProxyDispatchResult {
                            error: Some(status),
                            response: std::sync::Arc::new(std::sync::Mutex::new(None)),
                            economics:
                                crate::core::execution_lifecycle::ProxyEconomicsObservation {
                                    tokens_pruned: 0,
                                    original_tokens: prepared.original_tokens,
                                    task_class: prepared.task_class.clone(),
                                },
                        };
                        return Ok((
                            ProxyProcessed {
                                result,
                                prepared: Some(prepared),
                                skip_reason: Some("upstream transport failed"),
                            },
                            crate::core::execution_lifecycle::StageDisposition::Skipped(
                                "upstream transport failed",
                            ),
                        ));
                    }
                };
                apply_response_headers(&mut response, &prepared);
                let result = proxy_dispatch_result(
                    response,
                    prepared.tokens_pruned,
                    prepared.original_tokens,
                    prepared.task_class.as_str(),
                );
                Ok((
                    ProxyProcessed {
                        result,
                        prepared: Some(prepared),
                        skip_reason: None,
                    },
                    crate::core::execution_lifecycle::StageDisposition::Applied,
                ))
            }
        }
    }

    async fn record_context_ir(
        &mut self,
        _context: &crate::core::execution_lifecycle::TaskContext,
        processed: &Self::Processed,
    ) -> Result<crate::core::execution_lifecycle::StageDisposition, Self::Error> {
        let Some(prepared) = processed.prepared.as_ref() else {
            return Ok(crate::core::execution_lifecycle::StageDisposition::Skipped(
                processed.skip_reason.unwrap_or("terminal result"),
            ));
        };
        let Some(intent) = prepared.context_ir.as_ref() else {
            return Ok(crate::core::execution_lifecycle::StageDisposition::Skipped(
                "Context IR unavailable for terminal result",
            ));
        };
        let kernel_result =
            crate::core::context_kernel::proxy_bridge::process_proxy_request(&intent.kernel_data);
        crate::core::context_kernel::envelope_wiring::process_proxy_evidence(
            &intent.kernel_data,
            &kernel_result,
        );
        #[cfg(feature = "enterprise")]
        if let Some(request) = intent.causal_request.as_ref() {
            if let Err(error) = crate::core::causal_attribution::record_proxy_context(
                &intent.causal_session_id,
                request,
                intent.causal_turn_provided,
            ) {
                tracing::debug!(%error, "causal attribution context recording failed");
            }
        }
        Ok(crate::core::execution_lifecycle::StageDisposition::Applied)
    }

    async fn record_ledger(
        &mut self,
        _context: &crate::core::execution_lifecycle::TaskContext,
        processed: &Self::Processed,
    ) -> Result<crate::core::execution_lifecycle::StageDisposition, Self::Error> {
        let Some(prepared) = processed.prepared.as_ref() else {
            return Ok(crate::core::execution_lifecycle::StageDisposition::Skipped(
                processed.skip_reason.unwrap_or("terminal result"),
            ));
        };
        super::super::determinism_guard::record_audit(
            &prepared.determinism_audit,
            !prepared.determinism_audit.is_stable,
        );
        if prepared.compression_candidate {
            prepared.state.stats.record_provider_request(
                &prepared.stats_label,
                prepared.original_size,
                prepared.compressed_size,
            );
        }
        if prepared.headroom_compatible {
            super::super::prefix_cache_stats::record_headroom_compat();
        }
        super::super::metrics::record_request(
            prepared.tokens_saved,
            prepared.compressed_size as u64,
        );
        if prepared.terminal.is_none() {
            super::super::cost::record(
                prepared.model.as_deref(),
                prepared.tokens_saved,
                prepared.original_size as u64,
                prepared.compressed_size as u64,
            );
        }
        if let Some((parsed, provider)) = prepared.introspect.as_ref() {
            let breakdown = super::super::introspect::analyze_request(parsed, *provider);
            prepared.state.introspect.record(breakdown);
        }
        if prepared.upstream_send_succeeded
            && let Some((conversation_id, forwarded, originals, message_count)) =
                prepared.prefix_replay.as_ref()
        {
            super::super::prefix_replay::record_forwarded(
                *conversation_id,
                forwarded.clone(),
                originals,
                *message_count,
            );
        }
        Ok(crate::core::execution_lifecycle::StageDisposition::Applied)
    }

    fn output_from_processed(&mut self, processed: Self::Processed) -> Self::Output {
        processed.result
    }

    fn observe(
        &self,
        result: &Result<
            Self::Output,
            crate::core::execution_lifecycle::LifecycleRunError<Self::Error>,
        >,
    ) -> crate::core::execution_lifecycle::CompletionObservation {
        let mut observation = result.as_ref().map_or_else(
            |_| {
                crate::core::execution_lifecycle::CompletionObservation::tool_result(
                    0, 0, "proxy", false,
                )
            },
            |result| {
                let success = result
                    .response
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .as_ref()
                    .is_some_and(|response| response.status().is_success());
                crate::core::execution_lifecycle::CompletionObservation::tool_result(
                    result.economics.original_tokens as u64,
                    0,
                    "proxy",
                    success,
                )
            },
        );
        if let Ok(result) = result {
            observation.proxy_economics = Some(result.economics.clone());
        }
        self.provider_label.clone_into(&mut observation.provider);
        observation.outcome_signals.clear();
        observation
    }
}
