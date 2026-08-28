---
name: frontend-verification
description: Verify changes to this frontend project with focused static checks, production builds, and a concise manual accessibility checklist.
disable-model-invocation: false
---

# Frontend verification

Run checks from the project root:

```bash
npm run lint
npm run build
```

Then review the changed UI for:

- keyboard reachability and visible focus;
- meaningful headings, labels, and alt text;
- narrow viewport behavior;
- loading, empty, and error states where relevant;
- no unexpected console errors.

Report the exact command, whether it passed, and any remaining manual checks.
