;;; lean-ctx.el --- Context intelligence layer for AI coding -*- lexical-binding: t; -*-

;; Author: lean-ctx <support@leanctx.com>
;; URL: https://github.com/yvgude/lean-ctx
;; Version: 1.0.0
;; Package-Requires: ((emacs "27.1"))
;; Keywords: tools, ai, context

;;; Commentary:

;; Thin client integration for the lean-ctx binary.
;; Provides statusline token savings display and commands
;; for setup, doctor, gain, and dashboard.
;;
;; Usage:
;;   (require 'lean-ctx)
;;   (lean-ctx-mode 1)
;;
;; Requires lean-ctx binary: cargo install lean-ctx

;;; Code:

(defgroup lean-ctx nil
  "Context intelligence layer for AI coding assistants."
  :group 'tools
  :prefix "lean-ctx-")

(defcustom lean-ctx-binary nil
  "Path to the lean-ctx binary. Auto-detected if nil."
  :type '(choice (const nil) string)
  :group 'lean-ctx)

(defcustom lean-ctx-refresh-interval 30
  "Seconds between stats refreshes."
  :type 'integer
  :group 'lean-ctx)

(defvar lean-ctx--binary-cache nil)
(defvar lean-ctx--stats-text "⚡lean-ctx")
(defvar lean-ctx--timer nil)

(defun lean-ctx--resolve-binary ()
  "Find the lean-ctx binary."
  (or lean-ctx-binary
      lean-ctx--binary-cache
      (setq lean-ctx--binary-cache
            (seq-find #'executable-find
                      '("lean-ctx"
                        "~/.cargo/bin/lean-ctx"
                        "/usr/local/bin/lean-ctx"
                        "/opt/homebrew/bin/lean-ctx"
                        "~/.local/bin/lean-ctx")))))

(defun lean-ctx--run-command (&rest args)
  "Run lean-ctx with ARGS synchronously, return output string."
  (let ((binary (lean-ctx--resolve-binary)))
    (unless binary
      (user-error "lean-ctx binary not found.  Install: cargo install lean-ctx"))
    (with-temp-buffer
      (let ((process-environment
             (append '("LEAN_CTX_ACTIVE=0" "NO_COLOR=1") process-environment)))
        (apply #'call-process binary nil t nil args))
      (buffer-string))))

(defun lean-ctx--run-command-async (callback &rest args)
  "Run lean-ctx with ARGS asynchronously, call CALLBACK with output."
  (let ((binary (lean-ctx--resolve-binary)))
    (unless binary
      (user-error "lean-ctx binary not found"))
    (let ((buf (generate-new-buffer " *lean-ctx*"))
          (process-environment
           (append '("LEAN_CTX_ACTIVE=0" "NO_COLOR=1") process-environment)))
      (set-process-sentinel
       (apply #'start-process "lean-ctx" buf binary args)
       (lambda (_proc _event)
         (with-current-buffer buf
           (funcall callback (buffer-string)))
         (kill-buffer buf))))))

(defun lean-ctx--format-tokens (n)
  "Format token count N for display."
  (cond
   ((>= n 1000000) (format "%.1fM" (/ n 1000000.0)))
   ((>= n 1000) (format "%.1fK" (/ n 1000.0)))
   (t (number-to-string n))))

(defun lean-ctx--stats-path ()
  "Return path to stats.json."
  (expand-file-name "~/.lean-ctx/stats.json"))

(defun lean-ctx--read-stats ()
  "Read stats.json and return alist or nil."
  (let ((path (lean-ctx--stats-path)))
    (when (file-exists-p path)
      (condition-case nil
          (json-read-file path)
        (error nil)))))

(defun lean-ctx--update-stats ()
  "Update the modeline stats text."
  (let ((stats (lean-ctx--read-stats)))
    (if (and stats (> (or (alist-get 'total_input_tokens stats) 0) 0))
        (setq lean-ctx--stats-text
              (format "⚡%s saved"
                      (lean-ctx--format-tokens
                       (alist-get 'total_input_tokens stats))))
      (setq lean-ctx--stats-text "⚡lean-ctx"))
    (force-mode-line-update t)))

;;;###autoload
(defun lean-ctx-setup ()
  "Run lean-ctx setup."
  (interactive)
  (lean-ctx--run-command-async #'message "setup"))

;;;###autoload
(defun lean-ctx-doctor ()
  "Run lean-ctx doctor."
  (interactive)
  (message "%s" (lean-ctx--run-command "doctor")))

;;;###autoload
(defun lean-ctx-gain ()
  "Show lean-ctx gain report."
  (interactive)
  (message "%s" (lean-ctx--run-command "gain")))

;;;###autoload
(defun lean-ctx-dashboard ()
  "Open lean-ctx dashboard in browser."
  (interactive)
  (lean-ctx--run-command-async (lambda (_) nil) "dashboard"))

;;;###autoload
(define-minor-mode lean-ctx-mode
  "Minor mode for lean-ctx status display."
  :global t
  :lighter (:eval (concat " " lean-ctx--stats-text))
  :group 'lean-ctx
  (if lean-ctx-mode
      (progn
        (lean-ctx--update-stats)
        (setq lean-ctx--timer
              (run-with-timer lean-ctx-refresh-interval
                              lean-ctx-refresh-interval
                              #'lean-ctx--update-stats)))
    (when lean-ctx--timer
      (cancel-timer lean-ctx--timer)
      (setq lean-ctx--timer nil))))

(provide 'lean-ctx)
;;; lean-ctx.el ends here
