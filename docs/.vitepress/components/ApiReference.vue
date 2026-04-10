<script setup lang="ts">
import { ApiReference } from '@scalar/api-reference'
import '@scalar/api-reference/style.css'

const configuration = {
  theme: 'purple',
  url: '/multilinguarr/openapi.json',
  hideDownloadButton: false,
}
</script>

<template>
  <ApiReference :configuration="configuration" />
</template>

<style scoped>
:deep(.scalar-app) {
  min-height: 80vh;
}
</style>
