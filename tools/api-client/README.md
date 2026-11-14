# API client type generation

This package isolates openapi-typescript's TypeScript 5 peer dependency from the
Angular 22 application's required TypeScript 6 dependency. Both use locked,
supported dependency trees; no peer dependency override is used.

Run `npm ci` and `npm run generate` here, or run `npm run api:generate` from the
frontend after installing this package. Generated files must not be edited.
