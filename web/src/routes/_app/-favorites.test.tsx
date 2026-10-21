import { afterEach, beforeEach, describe, expect, jest, spyOn, test } from 'bun:test'
import {
  focusManager,
  notifyManager,
  QueryClient,
  QueryClientProvider
} from '@tanstack/react-query'
import { Window } from 'happy-dom'
import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'

import { CACHE } from '@/lib/constants'
import { queryKeys } from '@/lib/query-keys'
import { Route } from './favorites'

const FavoritesPage = Route.options.component!
const favoritesKey = queryKeys.favorites(1)
const emptyFavorites = { items: [], total: 0 }

let dom: Window
let root: Root
let queryClient: QueryClient
let requestedUrls: string[]
let globals: Map<string, PropertyDescriptor | undefined>

beforeEach(() => {
  dom = new Window({ url: 'http://localhost' })
  const values = {
    window: dom,
    document: dom.document,
    navigator: dom.navigator,
    requestAnimationFrame: dom.requestAnimationFrame.bind(dom),
    cancelAnimationFrame: dom.cancelAnimationFrame.bind(dom),
    IS_REACT_ACT_ENVIRONMENT: true
  }
  globals = new Map(
    Object.keys(values).map(key => [key, Object.getOwnPropertyDescriptor(globalThis, key)])
  )
  for (const [key, value] of Object.entries(values)) {
    Object.defineProperty(globalThis, key, { value, configurable: true, writable: true })
  }
  const container = document.createElement('div')
  document.body.append(container)
  root = createRoot(container)
  queryClient = new QueryClient({
    defaultOptions: {
      queries: {
        staleTime: CACHE.LIST_STALE_TIME,
        gcTime: Infinity,
        refetchOnMount: false,
        refetchOnWindowFocus: false,
        retry: false
      }
    }
  })
  focusManager.setFocused(true)
  notifyManager.setNotifyFunction(callback => {
    void act(callback)
  })
  requestedUrls = []
  const mockFetch = Object.assign(
    async (input: Parameters<typeof fetch>[0]) => {
      requestedUrls.push(String(input))
      return Response.json(emptyFavorites)
    },
    { preconnect: fetch.preconnect }
  )
  spyOn(globalThis, 'fetch').mockImplementation(mockFetch)
})

afterEach(async () => {
  await act(async () => root.unmount())
  queryClient.clear()
  focusManager.setFocused(undefined)
  await dom.happyDOM.close()
  notifyManager.setNotifyFunction(callback => callback())
  jest.restoreAllMocks()
  for (const [key, descriptor] of globals) {
    if (descriptor) Object.defineProperty(globalThis, key, descriptor)
    else Reflect.deleteProperty(globalThis, key)
  }
})

async function render(visible = true) {
  await act(async () => {
    root.render(
      <QueryClientProvider client={queryClient}>
        {visible ? <FavoritesPage /> : null}
      </QueryClientProvider>
    )
  })
}

async function focus() {
  await act(async () => {
    focusManager.setFocused(false)
    focusManager.setFocused(true)
  })
}

describe('favorites query refresh policy', () => {
  test('refreshes a cached list older than ten seconds when entering the page', async () => {
    const updatedAt = Date.now() - 20_000
    queryClient.setQueryData(favoritesKey, emptyFavorites, { updatedAt })
    await render()
    expect(requestedUrls).toEqual(['/api/favorites?page=1'])
    expect(queryClient.getQueryState(favoritesKey)?.dataUpdatedAt).toBeGreaterThan(updatedAt)
  })

  test('refreshes an inactive list invalidated by a local favorite mutation on return', async () => {
    queryClient.setQueryData(favoritesKey, emptyFavorites)
    await render()
    expect(requestedUrls).toHaveLength(0)
    await render(false)
    await queryClient.invalidateQueries({ queryKey: queryKeys.favorites() })
    expect(requestedUrls).toHaveLength(0)
    await render()
    expect(requestedUrls).toEqual(['/api/favorites?page=1'])
    expect(queryClient.getQueryState(favoritesKey)?.isInvalidated).toBe(false)
  })

  test('refreshes a stale mounted list when the window regains focus', async () => {
    queryClient.setQueryData(favoritesKey, emptyFavorites)
    await render()
    await act(async () => {
      queryClient.setQueryData(favoritesKey, emptyFavorites, { updatedAt: Date.now() - 20_000 })
    })
    expect(requestedUrls).toHaveLength(0)
    await focus()
    expect(requestedUrls).toEqual(['/api/favorites?page=1'])
  })

  test('reuses fresh cached data on mount and focus', async () => {
    queryClient.setQueryData(favoritesKey, emptyFavorites)
    await render()
    await focus()
    await render(false)
    await render()
    await focus()
    expect(requestedUrls).toHaveLength(0)
  })
})
