/**
 * One fake alarm for board development, behind `?demo` in dev builds only.
 * Production never sees it: `import.meta.env.DEV` is false there, and the
 * island only opts in when the query flag is present. Everything is labeled
 * Demo where it renders.
 */
import type { Alarm, AlarmDetail } from './bindings/oarfish';

export const DEMO_ALARM: Alarm = {
  id: 'demo-alarm-01',
  template_id: 't_8f3c21a9',
  template:
    'EXT4-fs error (device <VAR:DEV>): ext4_find_entry:<VAR:NUM>: inode #<VAR:NUM>: ' +
    'comm <VAR:PROC>: reading directory lblock <VAR:NUM>',
  severity: 'major',
  host: 'nas',
  lane: 'Dashboard',
  count: 14,
  opened_at: '2026-09-19T03:12:04Z',
};

export const DEMO_DETAIL: AlarmDetail = {
  alarm: DEMO_ALARM,
  verdict: {
    template_id: 't_8f3c21a9',
    questions_hash: '9f2c4a1b5d6e7f809a1b2c3d4e5f6071',
    model: 'typesafe/jev-1.13-20260917',
    answers: {
      kind: {
        choice: {
          choice: 'hardware_fault',
          confidence: 0.81,
          probabilities: {
            hardware_fault: 0.81,
            software_error: 0.11,
            config: 0.06,
            routine: 0.02,
          },
        },
      },
      severity: {
        score: {
          score: 2.1,
          confidence: 0.77,
          probabilities: { '2': 0.72, '3': 0.2, '1': 0.08 },
          legend: { '0': 'noise', '1': 'degraded', '2': 'broken', '3': 'down' },
        },
      },
      actionable: { noul: { noul: 0.9 } },
      transient: { noul: { noul: 0.1 } },
    },
    judged_at: '2026-09-19T03:13:31Z',
  },
  record: {
    model: 'typesafe/jev-1.13-20260917',
    recorded_at: '2026-09-19T03:13:31Z',
    input_tokens: 1840,
    output_tokens: 96,
    cost: 0.0021,
  },
};
