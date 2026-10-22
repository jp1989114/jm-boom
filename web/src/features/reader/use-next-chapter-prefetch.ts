import { useQueryClient } from '@tanstack/react-query'
import { useEffect } from 'react'

import { ApiError } from '@/lib/api/client'
import { getComicReadManifest } from '@/lib/api/reader'
import { CACHE, READER } from '@/lib/constants'
import { queryKeys } from '@/lib/query-keys'
import { clearReaderPreloadScope, setReaderPreloadScope } from '@/lib/reader-preload'
import type { ReaderChapterItem } from './types'

const RETRY_DELAYS = [5000, 10_000]

export function useNextChapterPrefetch({
  currentIndex,
  pageCount,
  nextChapter,
  stripPrefetchRequested = false
}: {
  currentIndex: number
  pageCount: number
  nextChapter: ReaderChapterItem | null
  stripPrefetchRequested?: boolean
}) {
  const queryClient = useQueryClient()
  const nextReadId = nextChapter?.id ?? ''
  const preloadScope = `next-chapter:${nextReadId}`
  const shouldPrefetch =
    nextReadId.length > 0 &&
    pageCount > 0 &&
    shouldPrefetchNextChapter(currentIndex, pageCount, stripPrefetchRequested)

  useEffect(() => {
    if (!shouldPrefetch) {
      return
    }

    let isActive = true
    let retryCount = 0
    let retryTimer: number | undefined

    async function prefetch() {
      try {
        const manifest = await queryClient.fetchQuery({
          queryKey: queryKeys.readerManifest(nextReadId),
          queryFn: () => getComicReadManifest({ readId: nextReadId }),
          staleTime: CACHE.READER_STALE_TIME,
          gcTime: CACHE.READER_GC_TIME,
          retry: false
        })
        if (!isActive) {
          return
        }

        setReaderPreloadScope(
          preloadScope,
          manifest.pages.slice(0, READER.PREFETCH_AHEAD_PAGES).map(page => page.path)
        )
      } catch (error) {
        if (!isActive) return

        const retryDelay = RETRY_DELAYS[retryCount]
        if (error instanceof ApiError && error.retryable && retryDelay !== undefined) {
          retryCount += 1
          retryTimer = window.setTimeout(() => void prefetch(), retryDelay)
        }

        if (import.meta.env.DEV) {
          console.debug('Reader next chapter prefetch failed', error)
        }
      }
    }

    void prefetch()

    return () => {
      isActive = false
      window.clearTimeout(retryTimer)
    }
  }, [nextReadId, preloadScope, queryClient, shouldPrefetch])

  useEffect(
    () => () => {
      if (nextReadId) {
        clearReaderPreloadScope(preloadScope)
      }
    },
    [nextReadId, preloadScope]
  )
}

export function shouldPrefetchNextChapter(
  currentIndex: number,
  pageCount: number,
  stripPrefetchRequested = false
) {
  const remainingPages = pageCount - currentIndex - 1
  const progress = pageCount > 0 ? (currentIndex + 1) / pageCount : 0

  return (
    stripPrefetchRequested ||
    remainingPages <= READER.PREFETCH_AHEAD_PAGES ||
    progress >= READER.NEXT_CHAPTER_PREFETCH_PROGRESS
  )
}
