import { createApp } from 'vue'
import { createRouter, createWebHistory } from 'vue-router'
import App from './App.vue'
import AgentView from './views/AgentView.vue'
import './style.css'

const PluginView = () => import('./views/PluginView.vue')

const router = createRouter({
  history: createWebHistory(),
  routes: [
    { path: '/', redirect: '/agent' },
    { path: '/chat', redirect: '/agent' },
    { path: '/apis', redirect: '/agent' },
    { path: '/agent', component: AgentView },
    { path: '/plugins', component: PluginView },
  ]
})

const app = createApp(App)
app.use(router)
app.mount('#app')
