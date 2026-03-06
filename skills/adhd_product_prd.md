# RustClaw ADHD Assistant PRD

## 1. Product Positioning
- Product name: RustClaw Focus Companion
- Goal: Help ADHD or ADHD-like users plan life, track progress, and get emotional support with low cognitive load.
- Core value: Reduce execution friction, increase completion probability, and maintain emotional safety.
- Primary channel: Telegram first, TUI compatible.

## 2. Target Users
- Diagnosed ADHD adults with high context-switching and procrastination.
- ADHD-like students with unstable routines and planning failures.
- High-stress users with attention fluctuation and emotional overwhelm.

## 3. Pain Points
- Plans are too big to start.
- Interruptions break momentum and recovery is hard.
- Shame spiral after missing tasks.
- Weak memory for commitments and no gentle follow-up.

## 4. Jobs To Be Done
- Break complex goals into immediately executable micro-steps.
- Recover quickly after distraction.
- Receive empathetic, non-judgmental guidance during low mood.
- End day with evidence of progress and a clear next step.

## 5. Product Goals and Boundaries
### Goals
- Improve daily executable completion rate.
- Reduce average task start latency.
- Shorten recovery time after emotional drops.

### Boundaries
- Not a medical diagnosis tool.
- Not a replacement for therapy.
- In self-harm or harm-risk contexts: prioritize safety guidance and hotline suggestions.

## 6. UX Principles
- One next action at a time.
- Anti-shame communication.
- Start first, optimize later.
- Reinforce progress evidence.
- Always resumable from interruption.

## 7. MVP Scope
### 7.1 Planning Guidance
- Convert goals into: Top 3 today, smallest next action, estimated duration.
- Modes: low-energy and standard.

### 7.2 Progress Tracking
- States: todo / doing / done / paused.
- Timestamped updates with short notes.

### 7.3 Focus Sprint
- 5/15/25 minute sprint start.
- On interruption, provide immediate re-entry action.

### 7.4 Emotional Support
- Empathy + one actionable suggestion.
- Ultra-low-threshold actions for low-energy moments.

### 7.5 End-of-Day Review
- Done evidence list.
- Friction summary.
- First step for tomorrow.

### 7.6 Reminder and Automation
- Cron-based reminders (wake up, meds, hydration, review).
- Telegram proactive nudges.

## 8. Milestones
### V1 (4 weeks)
- Telegram planning, tracking, and review loop.
- Baseline emotional support style and safety prompts.

### V1.5 (6-8 weeks)
- Personal rhythm learning (best time windows and duration).
- Reminder escalation and quiet window.

### V2 (8-12 weeks)
- Optional voice input.
- Weekly and monthly trend insights.
- Optional buddy/family collaboration.

## 9. Core Flows
### Onboarding
- Select mode: companion / planning / emotional support.
- Capture preferences: schedule, energy peaks, reminder windows.
- Generate today plan and confirm first action.

### Daily Execution
- User starts task via message.
- Assistant runs sprint and checks back.
- Dynamic reprioritization on status changes.

### Day Closure
- Assistant triggers review Q&A.
- Creates tomorrow first action and reminder.

## 10. Metrics
### North Star
- Active execution days in rolling 7 days (>=1 completed planned action per day).

### Supporting
- First-task start latency.
- Median completed tasks per day.
- Post-interruption recovery rate.
- End-of-day review completion rate.
- Emotional-support-to-action conversion rate.

## 11. Safety and Communication Rules
### Must
- Always provide one smallest next step + one time box.
- On failure, default to support and restart path.

### Must Not
- Shame, blame, or moral pressure language.
- Framing ADHD struggles as lack of discipline.

### High-Risk Handling
- Detect crisis cues and switch to safety-first messaging with help resources.

## 12. RustClaw Mapping
- `channel.telegram`: primary interaction and proactive nudges.
- `memory`: user preferences, trigger patterns, effective strategies.
- `skills`: ADHD coaching prompts and templates.
- `cron`: scheduled reminders and daily review triggers.
- `session`: continuity of plan and state.

## 13. MVP Acceptance Criteria
- User can complete full loop in Telegram: plan -> execute -> review.
- At least three task state updates are supported and queryable.
- Reminders trigger correctly without spamming.
- Emotional support follows empathy + action format.
- Logs can trace plan status transitions and push outcomes.

## 14. Tone Guide
### Recommended
- "You already started. Let's do a 3-minute version first."
- "No need to finish it perfectly. Open the doc and write just the title."

### Avoid
- "You are not disciplined enough."
- "This is easy, why can't you do it?"
